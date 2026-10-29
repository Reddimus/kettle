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

const ACCESSIBILITY_ROOT_ID: NodeId = NodeId(0);
const ACCESSIBILITY_PANE_ID_MASK: u64 = 1 << 63;
const ACCESSIBILITY_TEXT_ID_MASK: u64 = (1 << 63) | (1 << 62);
const ACCESSIBILITY_SEARCH_ID_MASK: u64 = 1 << 61;
const ACCESSIBILITY_SEARCH_CONTAINER_ID: NodeId = NodeId(ACCESSIBILITY_SEARCH_ID_MASK);
const ACCESSIBILITY_SEARCH_TEXT_ID: NodeId = NodeId(ACCESSIBILITY_SEARCH_ID_MASK | 16);
const ACCESSIBILITY_SEARCH_STATUS_ID: NodeId = NodeId(ACCESSIBILITY_SEARCH_ID_MASK | 17);
const ACCESSIBILITY_COMPLETION_ID_MASK: u64 = 1 << 60;
const ACCESSIBILITY_COMPLETION_CONTAINER_ID: NodeId = NodeId(ACCESSIBILITY_COMPLETION_ID_MASK);
const ACCESSIBILITY_MEDIA_RECEIPT_ID: NodeId = NodeId(1 << 59);
/// The transient update status.
const ACCESSIBILITY_UPDATE_STATUS_ID: NodeId = NodeId((1 << 59) + 2);
const ACCESSIBILITY_UPDATE_DETAILS_ID: NodeId = NodeId((1 << 59) + 3);
const ACCESSIBILITY_UPDATE_DISMISS_ID: NodeId = NodeId((1 << 59) + 4);
const ACCESSIBILITY_MEDIA_RECEIPT_DISMISS_ID: NodeId = NodeId((1 << 59) | 1);
const ACCESSIBILITY_MODAL_ID_MASK: u64 = 1 << 58;
/// Inline cards on screen, each by its placement's instance number, which
/// stays below this bit; never by its nonce.
const ACCESSIBILITY_CARD_ID_MASK: u64 = crate::media::MAX_CARD_INSTANCE;
const ACCESSIBILITY_UPDATE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

fn accessibility_completion_geometry_key(
    completion: &kettle_render::CompletionOverlay,
    cell: (f32, f32),
) -> u64 {
    use std::hash::{Hash as _, Hasher as _};

    fn hash_rect(rect: kettle_render::Rect4, hasher: &mut impl std::hash::Hasher) {
        for component in [rect.0, rect.1, rect.2, rect.3] {
            component.to_bits().hash(hasher);
        }
    }

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    if let Some(rect) = kettle_render::completion_overlay_rect(completion, cell) {
        true.hash(&mut hasher);
        hash_rect(rect, &mut hasher);
    } else {
        false.hash(&mut hasher);
    }
    for (index, rect) in kettle_render::completion_overlay_row_rects(completion, cell) {
        index.hash(&mut hasher);
        hash_rect(rect, &mut hasher);
    }
    hasher.finish()
}

fn completion_overlay_hit_test(
    completion: Option<&kettle_render::CompletionOverlay>,
    cell: (f32, f32),
    point: (f32, f32),
) -> bool {
    completion
        .and_then(|completion| kettle_render::completion_overlay_rect(completion, cell))
        .is_some_and(|(x, y, width, height)| {
            point.0 >= x && point.0 < x + width && point.1 >= y && point.1 < y + height
        })
}

fn clear_completions_on_focus_loss<T>(
    focused: bool,
    panes: impl IntoIterator<Item = T>,
    mut clear: impl FnMut(T),
) {
    if !focused {
        for pane in panes {
            clear(pane);
        }
    }
}

fn warn_refused_terminal_url(_uri: &str, warn: impl FnOnce(&str)) {
    warn("refused to open an unsafe URL from terminal output");
}

fn apply_pane_exit_action(
    action: kettle_config::ExitAction,
    pane_id: u64,
    pane: &mut Pane,
    pending_restarts: &mut Vec<u64>,
) {
    if pane.exit_observed {
        return;
    }
    pane.exit_observed = true;
    match action {
        kettle_config::ExitAction::Hold => {
            pane.held = true;
            // `try_wait` is now safe: the ordered PTY exit (or the bounded
            // fallback below) has finished consuming output. Cache the status
            // and release the Unix zombie while the held grid stays visible.
            pane.held_child_reaped = pane.term.child_exit_code().is_some();
        }
        kettle_config::ExitAction::Restart => {
            if !pending_restarts.contains(&pane_id) {
                pending_restarts.push(pane_id);
                log::info!("exit-action = restart: queued pane {pane_id} for respawn");
            }
            pane.closed = true;
        }
        kettle_config::ExitAction::Close => pane.closed = true,
    }
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoundedPtyExitAction {
    Wait,
    BeginClose,
    ApplyPolicy,
}

#[cfg(any(windows, test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum BoundedPtyExitPhase {
    NotStarted,
    InProgress(std::time::Duration),
    Failed { retry_in: std::time::Duration },
}

#[cfg(any(windows, test))]
fn bounded_pty_exit_action(
    child_exited_for: std::time::Duration,
    phase: BoundedPtyExitPhase,
) -> BoundedPtyExitAction {
    match phase {
        BoundedPtyExitPhase::NotStarted
            if child_exited_for >= kettle_core::term::PTY_CHILD_EXIT_EOF_TIMEOUT =>
        {
            // Even a badly delayed/coalesced first wake must start close before
            // policy. Applying Hold while the live master is still owned would
            // make the reader permanent.
            BoundedPtyExitAction::BeginClose
        }
        BoundedPtyExitPhase::InProgress(close_for)
            if close_for >= kettle_core::term::PTY_CHILD_EXIT_EOF_TIMEOUT =>
        {
            BoundedPtyExitAction::ApplyPolicy
        }
        BoundedPtyExitPhase::Failed { retry_in } if retry_in.is_zero() => {
            BoundedPtyExitAction::BeginClose
        }
        _ => BoundedPtyExitAction::Wait,
    }
}

fn take_window_lifecycle_wakes(ws: &WindowState) -> bool {
    // Do not use `Iterator::any`: its short-circuit would leave later panes'
    // edges set and, because the process-wide Wakeup was already coalesced,
    // could strand their semantic events without another turn.
    let mut pending = false;
    for pane in ws.mux.panes.values() {
        pending |= pane.term.take_lifecycle_wake();
    }
    pending
}

#[cfg(windows)]
fn next_bounded_pty_exit_wait(
    ws: &WindowState,
    now: std::time::Instant,
) -> Option<std::time::Duration> {
    ws.mux
        .panes
        .values()
        .filter(|pane| !pane.exit_observed)
        .filter_map(|pane| {
            let child_exit_at = pane.term.direct_child_exit_at()?;
            let deadline = match pane.pty_output_close_phase {
                PtyOutputClosePhase::NotStarted => {
                    child_exit_at + kettle_core::term::PTY_CHILD_EXIT_EOF_TIMEOUT
                }
                PtyOutputClosePhase::InProgress(started) => {
                    started + kettle_core::term::PTY_CHILD_EXIT_EOF_TIMEOUT
                }
                PtyOutputClosePhase::Failed { retry_at } => retry_at,
            };
            Some(
                deadline
                    .saturating_duration_since(now)
                    .max(std::time::Duration::from_millis(1)),
            )
        })
        .min()
}

fn osc52_copy_is_available(policy: Osc52, clipboard_available: bool) -> bool {
    policy.can_copy() && clipboard_available
}

#[cfg(any(target_os = "macos", test))]
fn macos_effective_modifiers(
    mut raw: ModifiersState,
    policy: MacosOptionAsAlt,
    left_option_pressed: bool,
    right_option_pressed: bool,
) -> ModifiersState {
    if !raw.alt_key() || raw.control_key() || raw.super_key() {
        return raw;
    }
    let option_is_alt = match policy {
        MacosOptionAsAlt::None => false,
        MacosOptionAsAlt::Left => left_option_pressed,
        MacosOptionAsAlt::Right => right_option_pressed,
        MacosOptionAsAlt::Both => true,
    };
    if !option_is_alt {
        raw.remove(ModifiersState::ALT);
    }
    raw
}

/// Whether `macos-option-as-alt` has any business masking Option for this key.
///
/// The policy answers exactly one question: does Option compose text (`⌥e` →
/// `´`) or act as Meta? That question does not arise for a key macOS composes
/// no character from. Answering it anyway lets `macos_effective_modifiers` drop
/// the ALT bit, so the encoder's `ESC DEL` arm (`input::encode`, the
/// `(false, true)` Backspace case) never sees it and `⌥⌫` deletes a single
/// character instead of a word.
///
/// Backspace is the one entry that cannot be derived. winit reports the legacy
/// C0 `\x08` from `NamedKey::Backspace.to_text()`, so a plain
/// `to_text().is_none()` test excludes the very key this exists for.
#[cfg(any(target_os = "macos", test))]
fn option_is_meta_for(key: &Key) -> bool {
    match key {
        Key::Named(NamedKey::Backspace) => true,
        // Return composes nothing either, though winit gives it the text
        // "\r". Option+Return is Alt+Return, the newline chord in Codex,
        // Claude Code, zsh and fish; masked, it would submit instead.
        Key::Named(NamedKey::Enter) => true,
        Key::Named(named) => named.to_text().is_none(),
        // Exactly what the policy protects. Restoring ALT for a character key
        // would prefix ESC to a glyph macOS has already composed, so `⌥e` would
        // send `ESC ´` instead of `´`.
        _ => false,
    }
}

/// The modifiers the PTY encoder should see, as against the ones the rest of
/// the UI sees. Identical to `masked` except on the keys `option_is_meta_for`
/// exempts, where a physically-held Option is restored.
///
/// The `left`/`right` policy sides are deliberately not consulted: a side that
/// cannot compose text has nothing to choose between. kitty draws the same
/// line — `macos_option_as_alt = no` still sends `ESC DEL` for `⌥⌫`, because
/// that setting governs only the text path.
#[cfg(any(target_os = "macos", test))]
fn macos_pty_modifiers(masked: ModifiersState, raw: ModifiersState, key: &Key) -> ModifiersState {
    if raw.alt_key() && option_is_meta_for(key) {
        return masked | ModifiersState::ALT;
    }
    masked
}

/// `ws.mods` as the PTY key encoder should read it. Every other consumer —
/// keybind matching above all — keeps the masked value, so no chord starts
/// firing an action it did not fire before.
///
/// Cross-platform on purpose, with the `cfg` inside the body: splitting at the
/// call site would leave a `ws.mods` in `write_terminal_key_event` for the
/// source census to find on every target.
fn pty_key_modifiers(ws: &WindowState, key: &Key) -> ModifiersState {
    #[cfg(target_os = "macos")]
    {
        macos_pty_modifiers(ws.mods, ws.macos_raw_mods, key)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = key;
        ws.mods
    }
}

/// Whether Option is physically down, whatever `macos-option-as-alt` says.
///
/// On macOS the search bar binds its word-wise motions to Option, and only on
/// Backspace, Delete and the horizontal arrows, all keys `option_is_meta_for`
/// exempts. So the composition policy has no say here either. Reading the
/// masked `ws.mods` would leave `delete_word_backward` and `move_left(by_word)`
/// unreachable.
fn option_physically_held(ws: &WindowState) -> bool {
    #[cfg(target_os = "macos")]
    {
        ws.macos_raw_mods.alt_key()
    }
    #[cfg(not(target_os = "macos"))]
    {
        ws.mods.alt_key()
    }
}

#[cfg(target_os = "macos")]
fn set_macos_option_as_alt(window: &Window, policy: MacosOptionAsAlt) {
    use winit::platform::macos::{OptionAsAlt, WindowExtMacOS as _};

    let policy = match policy {
        MacosOptionAsAlt::None => OptionAsAlt::None,
        MacosOptionAsAlt::Left => OptionAsAlt::OnlyLeft,
        MacosOptionAsAlt::Right => OptionAsAlt::OnlyRight,
        MacosOptionAsAlt::Both => OptionAsAlt::Both,
    };
    window.set_option_as_alt(policy);
}

#[cfg(target_os = "macos")]
fn refresh_macos_modifiers(ws: &mut WindowState, policy: MacosOptionAsAlt) {
    ws.mods = macos_effective_modifiers(
        ws.macos_raw_mods,
        policy,
        ws.macos_left_option_pressed,
        ws.macos_right_option_pressed,
    );
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Osc52ClipboardChannel {
    Clipboard,
    #[cfg(target_os = "linux")]
    Primary,
}

fn osc52_clipboard_channel(target: ClipboardType) -> Osc52ClipboardChannel {
    #[cfg(target_os = "linux")]
    if target == ClipboardType::Selection {
        return Osc52ClipboardChannel::Primary;
    }

    let _ = target;
    Osc52ClipboardChannel::Clipboard
}

fn prepare_osc52_copy(
    text: String,
    cap: usize,
) -> Result<String, std::collections::TryReserveError> {
    let mut text = kettle_core::scrub_card_markers(text)?;
    let keep = clamp_osc52(&text, cap).len();
    text.truncate(keep);
    Ok(text)
}

fn set_osc52_clipboard(
    clipboard: &mut arboard::Clipboard,
    target: ClipboardType,
    text: String,
) -> Result<(), arboard::Error> {
    match osc52_clipboard_channel(target) {
        Osc52ClipboardChannel::Clipboard => clipboard.set_text(text),
        #[cfg(target_os = "linux")]
        Osc52ClipboardChannel::Primary => {
            use arboard::{LinuxClipboardKind, SetExtLinux};
            clipboard
                .set()
                .clipboard(LinuxClipboardKind::Primary)
                .text(text)
        }
    }
}

fn get_osc52_clipboard(
    clipboard: &mut arboard::Clipboard,
    target: ClipboardType,
) -> Result<String, arboard::Error> {
    match osc52_clipboard_channel(target) {
        Osc52ClipboardChannel::Clipboard => clipboard.get_text(),
        #[cfg(target_os = "linux")]
        Osc52ClipboardChannel::Primary => {
            use arboard::{GetExtLinux, LinuxClipboardKind};
            clipboard
                .get()
                .clipboard(LinuxClipboardKind::Primary)
                .text()
        }
    }
}

fn apply_output_generation_outcome(
    outcome: FrameOutcome,
    committed: &mut std::collections::HashMap<u64, u64>,
    candidate: &mut std::collections::HashMap<u64, u64>,
) -> bool {
    if outcome != FrameOutcome::Presented {
        return false;
    }
    std::mem::swap(committed, candidate);
    true
}

fn stage_output_generations_for_frame(
    candidate: &mut std::collections::HashMap<u64, u64>,
    committed: &std::collections::HashMap<u64, u64>,
    cached_snapshot_keys: Option<&[(u64, u64, usize, usize)]>,
    current: impl IntoIterator<Item = (u64, u64)>,
) {
    candidate.clear();
    if let Some(cached_snapshot_keys) = cached_snapshot_keys {
        // A hover frame represents the cached active layout. Preserve the
        // already-consumed generations for background panes, then overwrite
        // visible panes with the exact generations in those snapshots. Output
        // racing on either kind of pane therefore remains pending.
        candidate.extend(committed.iter().map(|(id, generation)| (*id, *generation)));
        candidate.extend(
            cached_snapshot_keys
                .iter()
                .map(|(id, generation, _, _)| (*id, *generation)),
        );
    } else {
        candidate.extend(current);
    }
}

/// Decide whether an output wake still represents unpainted damage.
///
/// A wake queued by output racing a frame can become stale when that same
/// frame presents the new generation before the event is dispatched. Rearm
/// the per-pane latch only in that no-damage case, then sample again: output
/// racing between the first sample and rearm is handled by this event, while
/// output arriving after rearm owns a fresh wake. A wake that is still dirty
/// deliberately leaves the latch closed across the deferred paint budget.
fn output_wakeup_needs_paint(
    mut has_new_output: impl FnMut() -> bool,
    rearm_stale_wakeup: impl FnOnce(),
) -> bool {
    if has_new_output() {
        return true;
    }
    rearm_stale_wakeup();
    has_new_output()
}

fn render_hidden_from_observations(
    occluded: bool,
    minimized: bool,
    window_shown: bool,
    visible: bool,
) -> bool {
    occluded || minimized || (window_shown && !visible)
}

/// The log target for the cursor blink layer: one line per window when it
/// falls back to GPU blink, and each exit frame's duration, printed only
/// under `RUST_LOG=warn,kettle::cursor_blink=info`.
const CURSOR_BLINK_LOG: &str = "kettle::cursor_blink";

/// Whether a theme schedule has a decision that the last redraw did not apply.
fn theme_schedule_redraw_due(last: Option<bool>, now: Option<bool>) -> bool {
    now.is_some() && last != now
}

/// The wait until the next whole wall-clock minute, when a clock or sunrise
/// schedule can next flip; never zero.
fn until_next_wall_minute() -> std::time::Duration {
    let into_minute = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() % 60_000)
        .unwrap_or(0) as u64;
    std::time::Duration::from_millis(60_000 - into_minute)
}

/// A modal whose input bar is an active text surface keeps the terminal
/// cursor steady, never mid-blink-off, while it is open.
fn modal_holds_cursor_steady(ws: &WindowState) -> bool {
    ws.ssh_input.is_some()
        || ws.palette_input.is_some()
        || ws.layout_picker_input.is_some()
        || ws.theme_picker_input.is_some()
        || ws.hint_state.is_some()
        || ws.search.open
        // The title-edit and confirm-dialog input bars are also active text
        // surfaces, like the other modals.
        || ws.editing_title.is_some()
        || ws.confirm_dialog.is_some()
        || ws.settings_nav.is_some()
}

/// A control request that only reads UI state, so it needs no frame. A
/// frame would also end the Core Animation cursor blink `ui_geometry` is
/// there to observe.
/// `get_state`'s `caller`: whether the caller runs in one of this Kettle's
/// panes. Verified means the pane's own child process is an ancestor of the
/// connecting process; the nearest such pane wins. A hint from the caller's
/// environment is reported separately and never makes it verified.
fn ctl_caller_report(
    caller: &crate::ctl_server::CallerEvidence,
    roots: &[crate::media::PaneRoot],
    panes: &[(u64, u64)],
    kettle_pid: u32,
) -> serde_json::Value {
    use kettle_ctl::identity::UnverifiedReason;
    let hint = caller
        .hint()
        .filter(|hint| hint.kettle_pid == kettle_pid)
        .and_then(|hint| panes.iter().find(|(pane, _)| *pane == hint.pane))
        .map(|(pane, window)| serde_json::json!({"pane": pane, "window": window}));
    let (chain, reason) = match caller.chain() {
        Some(Ok(chain)) => (Some(chain), None),
        Some(Err(reason)) => (None, Some(reason)),
        // Not checked: only requests that report their caller are.
        None => (None, Some(UnverifiedReason::MissingClaim)),
    };
    let found = chain.and_then(|chain| crate::media::nearest_pane(chain, roots));
    let reason = reason.or_else(|| found.is_none().then_some(UnverifiedReason::NoPane));
    let peer = chain.and_then(|chain| chain.first()).map(|peer| {
        serde_json::json!({
            "pid": peer.pid(),
            "start_token": peer.start().to_string(),
        })
    });
    serde_json::json!({
        "verified": found.is_some(),
        "reason": reason.map(UnverifiedReason::as_str),
        "pane": found.map(|(pane, _)| pane),
        "window": found.map(|(_, window)| window),
        "peer": peer,
        "hint": hint,
    })
}

/// The control-server launch flags.
fn ctl_overrides(startup: &crate::Options) -> kettle_ctl::CtlOverrides {
    kettle_ctl::CtlOverrides {
        server: startup.agent_server,
        display: startup.agent_display,
    }
}

/// Where the control server's start stands. It may start once the first pane
/// exists, and tries at most once unless display is newly turned on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CtlStart {
    NotReady,
    Ready,
    Attempted,
}

fn ctl_method_is_pure_read(method: kettle_ctl::protocol::Method) -> bool {
    matches!(
        method,
        kettle_ctl::protocol::Method::UiGeometry
            | kettle_ctl::protocol::Method::AgentLaunch
            | kettle_ctl::protocol::Method::CardPickup
    )
}

fn window_is_render_hidden(ws: &WindowState) -> bool {
    ws.window.as_ref().is_some_and(|window| {
        render_hidden_from_observations(
            ws.window_occluded,
            window.is_minimized().unwrap_or(false),
            ws.window_shown,
            window.is_visible().unwrap_or(true),
        )
    }) || ws.window_occluded
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ScreenshotWindowAvailability {
    Available,
    Unavailable,
    /// The backend cannot report visibility/minimization (notably Wayland).
    /// Capture remains eligible, but its bounded timeout is the only fallback
    /// if the compositor withholds work for a state the client cannot observe.
    Unknown,
}

fn screenshot_window_availability(
    minimized: Option<bool>,
    window_shown: bool,
    visible: Option<bool>,
) -> ScreenshotWindowAvailability {
    if !window_shown || minimized == Some(true) || visible == Some(false) {
        ScreenshotWindowAvailability::Unavailable
    } else if minimized.is_none() || visible.is_none() {
        ScreenshotWindowAvailability::Unknown
    } else {
        ScreenshotWindowAvailability::Available
    }
}

fn screenshot_window_accepts_control_capture(
    minimized: Option<bool>,
    window_shown: bool,
    visible: Option<bool>,
) -> bool {
    screenshot_window_availability(minimized, window_shown, visible)
        != ScreenshotWindowAvailability::Unavailable
}

/// Parse an optional explicit full-window screenshot crop. The four values are
/// one unit: accepting a partial rectangle would make a typo silently capture
/// more terminal content than the caller intended.
fn screenshot_crop(
    params: &serde_json::Value,
) -> Result<Option<(f32, f32, f32, f32)>, &'static str> {
    const KEYS: [&str; 4] = ["crop_x", "crop_y", "crop_width", "crop_height"];
    let present = KEYS.map(|key| params.get(key).is_some());
    if !present.into_iter().any(|value| value) {
        return Ok(None);
    }
    if !present.into_iter().all(|value| value) {
        return Err("crop_x, crop_y, crop_width, and crop_height must be provided together");
    }
    let read = |key| {
        params
            .get(key)
            .and_then(serde_json::Value::as_f64)
            .filter(|value| value.is_finite() && *value <= f64::from(f32::MAX))
            .map(|value| value as f32)
            .ok_or("screenshot crop values must be finite numbers within the f32 range")
    };
    let crop = (
        read("crop_x")?,
        read("crop_y")?,
        read("crop_width")?,
        read("crop_height")?,
    );
    if crop.0 < 0.0 || crop.1 < 0.0 {
        return Err("crop_x and crop_y must be non-negative");
    }
    if crop.2 <= 0.0 || crop.3 <= 0.0 {
        return Err("crop_width and crop_height must be positive");
    }
    Ok(Some(crop))
}

fn pending_screenshot_may_render_offscreen(
    pending: bool,
    minimized: Option<bool>,
    window_shown: bool,
    visible: Option<bool>,
) -> bool {
    pending && screenshot_window_accepts_control_capture(minimized, window_shown, visible)
}

fn output_wakeup_must_quiesce(
    render_hidden: bool,
    renderer_repair_pending: bool,
    renderer_unavailable: bool,
) -> bool {
    render_hidden || renderer_repair_pending || renderer_unavailable
}

fn output_wake_transport_enabled(render_quiesced: bool, output_sidechannel: bool) -> bool {
    !render_quiesced || output_sidechannel
}

fn output_generation_advanced(previous: Option<u64>, current: u64) -> bool {
    previous.is_some_and(|previous| previous != current)
}

fn retain_renderer_recovery_snapshot<T>(retained: &mut Option<T>, live: Option<T>) {
    if retained.is_none() {
        *retained = live;
    }
}

fn accessibility_pane_id(pane_id: u64) -> NodeId {
    NodeId::from(ACCESSIBILITY_PANE_ID_MASK | pane_id)
}

fn accessibility_text_id(pane_id: u64) -> NodeId {
    NodeId::from(ACCESSIBILITY_TEXT_ID_MASK | pane_id)
}

fn accessibility_completion_row_id(position: usize) -> NodeId {
    NodeId(ACCESSIBILITY_COMPLETION_ID_MASK | (position as u64 + 1))
}

fn accessibility_search_control_id(control: kettle_render::SearchControl) -> NodeId {
    let offset = match control {
        kettle_render::SearchControl::Editor => 1,
        kettle_render::SearchControl::Previous => 2,
        kettle_render::SearchControl::Next => 3,
        kettle_render::SearchControl::Wrap => 4,
        kettle_render::SearchControl::Case => 5,
        kettle_render::SearchControl::Invert => 6,
        kettle_render::SearchControl::Close => 7,
    };
    NodeId(ACCESSIBILITY_SEARCH_ID_MASK | offset)
}

fn accessibility_search_control_from_id(id: NodeId) -> Option<kettle_render::SearchControl> {
    match id.0 {
        value
            if value == accessibility_search_control_id(kettle_render::SearchControl::Editor).0 =>
        {
            Some(kettle_render::SearchControl::Editor)
        }
        value
            if value
                == accessibility_search_control_id(kettle_render::SearchControl::Previous).0 =>
        {
            Some(kettle_render::SearchControl::Previous)
        }
        value if value == accessibility_search_control_id(kettle_render::SearchControl::Next).0 => {
            Some(kettle_render::SearchControl::Next)
        }
        value if value == accessibility_search_control_id(kettle_render::SearchControl::Wrap).0 => {
            Some(kettle_render::SearchControl::Wrap)
        }
        value if value == accessibility_search_control_id(kettle_render::SearchControl::Case).0 => {
            Some(kettle_render::SearchControl::Case)
        }
        value
            if value == accessibility_search_control_id(kettle_render::SearchControl::Invert).0 =>
        {
            Some(kettle_render::SearchControl::Invert)
        }
        value
            if value == accessibility_search_control_id(kettle_render::SearchControl::Close).0 =>
        {
            Some(kettle_render::SearchControl::Close)
        }
        _ => None,
    }
}

/// A sender line naming `program` and its `signer`, with where each sits in
/// it. The line is rendered once more per part with a control character in
/// that part's place, which no sanitized text holds, to find it in any
/// language's word order.
fn media_viewer_sender_line(
    line: impl Fn(&str, Option<&str>) -> String,
    program: &str,
    signer: Option<&str>,
) -> kettle_render::MediaLaneSender {
    let text = line(program, signer);
    let find = |marked: String, marker: char, part: &str| {
        marked
            .find(marker)
            .map(|start| start..start + part.len())
            .filter(|range| text.get(range.clone()) == Some(part))
    };
    let program_range = find(line("\u{1}", signer), '\u{1}', program);
    let signer_range =
        signer.and_then(|signer| find(line(program, Some("\u{2}")), '\u{2}', signer));
    kettle_render::MediaLaneSender {
        text,
        program: program_range,
        signer: signer_range,
    }
}

/// The mode a lane shows `item` in: its source only while it has a source to
/// show, as after a replacement by key with a raster it may not.
fn lane_mode(
    item: &crate::media::ShelfItem,
    panel: &crate::window_state::PreviewPanel,
) -> kettle_render::MediaLaneMode {
    match item.source.text() {
        Some(_) => panel.mode,
        None => kettle_render::MediaLaneMode::Rendered,
    }
}

/// Whether a lane showing `item` as `panel` says offers a silent preview: a
/// video with a length, shown rendered, whose source the lane can read
/// again.
fn lane_previews_item(
    item: &crate::media::ShelfItem,
    panel: &crate::window_state::PreviewPanel,
) -> Option<()> {
    let info = item.video?;
    (info.duration_ms > 0
        && item.kind == crate::media::ItemKind::Media(kettle_media::MediaKind::Video)
        && lane_mode(item, panel) == kettle_render::MediaLaneMode::Rendered
        && !matches!(item.source.spec.input, crate::media::SourceInput::Released))
    .then_some(())
}

fn lane_frame(
    item: &crate::media::ShelfItem,
    panel: &crate::window_state::PreviewPanel,
    preview: Option<&crate::media::LanePreview>,
    playback: Option<&crate::media::LanePlayback>,
) -> Option<kettle_render::MediaLaneFrame> {
    lane_previews_item(item, panel)?;
    if let Some(preview) =
        preview.filter(|preview| preview.serves(item.id, item.generation) && preview.active())
    {
        return preview.frame().map(|frame| kettle_render::MediaLaneFrame {
            sheet: frame.sheet.clone(),
            source: frame.source,
            size: item.size,
        });
    }
    let frame = playback
        .filter(|playback| playback.serves(item.id, item.generation))?
        .frame()?;
    Some(kettle_render::MediaLaneFrame {
        sheet: frame.clone(),
        source: (0, 0, frame.width, frame.height),
        size: item.size,
    })
}

fn lane_copy_image(
    item: &crate::media::ShelfItem,
    frame: Option<kettle_render::MediaLaneFrame>,
) -> Option<kettle_core::ImageData> {
    let Some(frame) = frame else {
        return item.image().cloned();
    };
    let (x, y, width, height) = frame.source;
    if (x, y, width, height) == (0, 0, frame.sheet.width, frame.sheet.height) {
        Some(frame.sheet)
    } else {
        frame.sheet.crop(x, y, width, height)
    }
}

/// The rest the real pointer makes on a lane's picture after `current`:
/// `hovered` is the lane whose picture it is on, at `at`, at `now`. A rest
/// is a pointer that stays put, so moving starts it again, until a loop
/// started from it; leaving ends it.
fn next_hover(
    current: Option<crate::window_state::LaneHover>,
    hovered: Option<u64>,
    at: Option<(f64, f64)>,
    now: std::time::Instant,
) -> Option<crate::window_state::LaneHover> {
    match (hovered, current) {
        (Some(pane), Some(hover)) if hover.pane == pane && (hover.at == at || hover.used) => {
            Some(hover)
        }
        (Some(pane), _) => Some(crate::window_state::LaneHover {
            pane,
            at,
            since: now,
            used: false,
        }),
        (None, _) => None,
    }
}

/// The earlier of two optional instants.
fn earliest(
    a: Option<std::time::Instant>,
    b: Option<std::time::Instant>,
) -> Option<std::time::Instant> {
    match (a, b) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (a, None) => a,
        (None, b) => b,
    }
}

/// Whether `item` can be read again: it came from a file, or its gallery
/// did, not from bytes a request carried or text copied.
fn preview_reloads(item: &crate::media::ShelfItem) -> bool {
    item.source.reload_file().is_some()
}

/// What a reload keeps of the item it replaces.
struct ReloadOf {
    key: crate::media::ShelfKey,
    title: String,
    canvas: kettle_media::Canvas,
    /// The gallery or document page shown, which the file is read again at.
    page: Option<crate::media::ReadPage>,
}

/// What reloading `item`, shown in `panel`, reads and keeps: its file, and
/// its key, title, the canvas the user chose for its lane, which a file
/// that now holds a diagram is rendered for, and a gallery's page shown.
/// None for bytes a request carried, text copied, or a path this platform
/// cannot name.
fn reload_of(
    item: &crate::media::ShelfItem,
    panel: &crate::window_state::PreviewPanel,
) -> Option<(std::path::PathBuf, ReloadOf)> {
    let (path, page) = item.source.reload_file()?;
    let native = native_path_buf(path)?;
    let reload = ReloadOf {
        key: item
            .key
            .clone()
            .unwrap_or_else(|| crate::media::ShelfKey::Path(path.clone())),
        title: item.title.clone(),
        canvas: media_canvas(chosen_canvas(item, panel)),
        page,
    };
    Some((native, reload))
}

/// The path a native path names on this platform.
fn native_path_buf(path: &kettle_media::NativePath) -> Option<std::path::PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt as _;
        Some(std::path::PathBuf::from(std::ffi::OsStr::from_bytes(
            path.as_bytes(),
        )))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt as _;
        let (units, rest) = path.as_bytes().as_chunks::<2>();
        if !rest.is_empty() {
            return None;
        }
        let wide: Vec<u16> = units.iter().map(|unit| u16::from_le_bytes(*unit)).collect();
        Some(std::path::PathBuf::from(std::ffi::OsString::from_wide(
            &wide,
        )))
    }
}

/// The shelf item `pane`'s lane shows in `ws`, if it shows one.
fn preview_item(ws: &WindowState, pane: u64) -> Option<&crate::media::ShelfItem> {
    let panel = ws.preview_panels.get(&pane)?;
    ws.mux
        .panes
        .get(&pane)?
        .media_shelf
        .items()
        .iter()
        .find(|item| item.id == panel.item)
}

/// What a lane's render of its item is worth when it comes back.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LaneRenderVerdict {
    /// The item left the shelf or was replaced, or the render is of a
    /// gallery page it no longer shows: the render is dropped.
    Gone,
    /// What was read is not what the item was rendered from.
    Changed,
    /// The same item from the same source, or the gallery page asked for:
    /// the render replaces its pixels.
    Same,
}

/// Judge a lane's render, which read a source with `digest`, against the
/// shelf `items` it asked about: the item's own source, or for another page
/// of its gallery, that page. Only a file read again can have changed;
/// bytes kept cannot, so a render of other bytes is of a page gone by.
fn lane_render_verdict(
    items: &[crate::media::ShelfItem],
    render: &crate::media::LaneRender,
    digest: &kettle_media::Digest,
) -> LaneRenderVerdict {
    let Some(item) = items
        .iter()
        .find(|item| item.id == render.item && item.generation == render.generation)
    else {
        return LaneRenderVerdict::Gone;
    };
    let expected = match render.page {
        Some(ask) => item.source.page_digest(ask.page),
        None => Some(item.source.digest.clone()),
    };
    if expected.as_ref() == Some(digest) {
        LaneRenderVerdict::Same
    } else if (render.page.is_none() || item.source.is_document()) && item.source.reads_file() {
        // Any page of a document reads the file again: a digest that is not
        // the document's is a file that changed.
        LaneRenderVerdict::Changed
    } else {
        LaneRenderVerdict::Gone
    }
}

/// The gallery page request `panel` waits on for its item at `generation`,
/// asked for last and not yet shown: what the lane will show, and where a
/// step counts from.
fn page_waited_on(
    panel: &crate::window_state::PreviewPanel,
    generation: u64,
) -> Option<crate::media::PageAsk> {
    panel
        .page
        .filter(|&(asked_of, _)| asked_of == generation)
        .map(|(_, ask)| ask)
}

/// Whether a lane with `panel` takes `render` of gallery page request `ask`,
/// its item showing page `shown`: the request it waits on, or one of the
/// page it shows drawn again on another canvas, for the item and generation
/// it shows. A request for the page it waits on that was asked again since
/// is not taken.
fn lane_takes_page(
    panel: &crate::window_state::PreviewPanel,
    render: &crate::media::LaneRender,
    ask: crate::media::PageAsk,
    shown: Option<u32>,
) -> bool {
    panel.item == render.item
        && (page_waited_on(panel, render.generation) == Some(ask) || shown == Some(ask.page))
}

/// The gallery page of `count` that `step` names: its first or last, or so
/// many on from the one asked for and not yet shown, else from the one
/// shown; `None` past the first or last, as a gallery does not wrap.
fn gallery_step(
    shown: u32,
    count: usize,
    asked: Option<u32>,
    step: crate::media::PageStep,
) -> Option<u32> {
    use crate::media::PageStep;
    let page = match step {
        PageStep::By(by) => i64::from(asked.unwrap_or(shown)).checked_add(i64::from(by))?,
        PageStep::First => 0,
        PageStep::Last => i64::try_from(count).ok()?.checked_sub(1)?,
    };
    usize::try_from(page)
        .is_ok_and(|page| page < count)
        .then(|| u32::try_from(page).ok())
        .flatten()
}

/// Most columns a source scrolls sideways: past any line a source can hold.
const MAX_SOURCE_COLUMN: usize = kettle_media::MAX_EXACT_SOURCE_BYTES;

/// `scroll` moved by `rows` and `columns`, kept on the source: from its
/// first row to its last of `total`, and from its first column.
fn scrolled_source(
    scroll: (usize, usize),
    rows: i32,
    columns: i32,
    total: usize,
) -> (usize, usize) {
    let step = |at: usize, by: i32, last: usize| {
        if by < 0 {
            at.saturating_sub(by.unsigned_abs() as usize)
        } else {
            at.saturating_add(by as usize)
        }
        .min(last)
    };
    (
        step(scroll.0, rows, total.saturating_sub(1)),
        step(scroll.1, columns, MAX_SOURCE_COLUMN),
    )
}

/// The rows and columns a lane's source moves for wheel `steps`: back
/// toward its start for the wheel up, and sideways for a horizontal swipe,
/// or the wheel with `shift` held. A positive horizontal step reveals what
/// is to the left, as winit's does; macOS turns a mouse wheel's motion with
/// Shift held into one.
fn source_scroll_by(steps: input::WheelSteps, shift: bool) -> (i32, i32) {
    let back = steps.lines.saturating_neg();
    let left = steps.cols.saturating_neg();
    if shift {
        (0, back.saturating_add(left))
    } else {
        (back, left)
    }
}

/// What the wheel does over a lane's rendered item.
#[derive(Clone, Copy, Debug, PartialEq)]
enum LaneWheel {
    /// Zoom at the pointer by this factor.
    Zoom(f64),
    /// Move the item by these pixels.
    Pan((f64, f64)),
}

/// How far, in logical pixels, a press on a lane's rendered item moves
/// before it pans the item.
const LANE_DRAG_SLOP: f64 = 3.0;

/// Whether a press at `from` moved to `at`, in physical pixels at `scale`,
/// is a drag.
fn past_drag_slop(from: (f64, f64), at: (f64, f64), scale: f64) -> bool {
    (at.0 - from.0).hypot(at.1 - from.1) >= LANE_DRAG_SLOP * scale
}

/// Most zoom steps one wheel event takes, so a burst a control client or a
/// device sends stays a zoom rather than a jump to an end.
const MAX_ZOOM_NOTCHES: f64 = 10.0;

/// What `delta` does over a lane's rendered item: zoom with `zooms` held
/// (Cmd on macOS, Ctrl elsewhere), a step per notch or per notch's worth of
/// a touchpad's pixels; otherwise pan, by a touchpad's pixels as they come
/// and by a wheel's lines a notch, in `cell`s, as the terminal scrolls.
/// Shift turns a wheel's vertical motion sideways.
fn lane_wheel(
    delta: &winit::event::MouseScrollDelta,
    zooms: bool,
    shift: bool,
    cell: (f32, f32),
    multiplier: f32,
) -> LaneWheel {
    use winit::event::MouseScrollDelta as D;
    let (notches, pan) = match *delta {
        D::LineDelta(x, y) => {
            let notches = f64::from(y);
            let (x, y) = (f64::from(x), f64::from(y));
            let (x, y) = if shift && x == 0.0 { (y, 0.0) } else { (x, y) };
            let lines = f64::from(input::LINES_PER_NOTCH * multiplier.max(0.0));
            (
                notches,
                (x * lines * f64::from(cell.0), y * lines * f64::from(cell.1)),
            )
        }
        D::PixelDelta(position) => (
            position.y / f64::from(input::PIXELS_PER_NOTCH),
            (position.x, position.y),
        ),
    };
    if zooms {
        LaneWheel::Zoom(
            kettle_render::MEDIA_ZOOM_STEP.powf(notches.clamp(-MAX_ZOOM_NOTCHES, MAX_ZOOM_NOTCHES)),
        )
    } else {
        LaneWheel::Pan(pan)
    }
}

/// Whether `mods` hold the key that turns the wheel over a lane's rendered
/// item into a zoom: Cmd on macOS, Ctrl elsewhere.
fn lane_zoom_modifier(mods: winit::keyboard::ModifiersState) -> bool {
    if cfg!(target_os = "macos") {
        mods.super_key()
    } else {
        mods.control_key()
    }
}

/// What whole wheel `steps` a control client sent do over a lane's
/// rendered item, as [`lane_wheel`] does with a wheel's lines: zoom a step
/// a notch with `zooms` held, or pan a row a line and three columns a
/// sideways notch, the lines sideways with Shift.
fn lane_wheel_steps(
    steps: input::WheelSteps,
    zooms: bool,
    shift: bool,
    cell: (f32, f32),
) -> LaneWheel {
    if zooms {
        let notches = f64::from(steps.notches).clamp(-MAX_ZOOM_NOTCHES, MAX_ZOOM_NOTCHES);
        return LaneWheel::Zoom(kettle_render::MEDIA_ZOOM_STEP.powf(notches));
    }
    let columns = f64::from(steps.cols) * f64::from(input::LINES_PER_NOTCH);
    let lines = f64::from(steps.lines);
    let (x, y) = if shift && steps.cols == 0 {
        (lines, 0.0)
    } else {
        (columns, lines)
    };
    LaneWheel::Pan((x * f64::from(cell.0), y * f64::from(cell.1)))
}

/// The zoom a pinch of `delta` makes, as winit counts it: positive
/// magnifies. Bounded, so no one event shrinks the item to nothing.
fn pinch_zoom(delta: f64) -> f64 {
    if delta.is_finite() {
        1.0 + delta.clamp(-0.5, 1.0)
    } else {
        1.0
    }
}

/// The part of `source` a lane shows from `scroll`
/// in `rows` rows of `columns` columns. A source replaced under its key may
/// be shorter than the one scrolled, so the scroll is kept on it.
fn lane_source(
    source: &crate::media::ItemSource,
    scroll: (usize, usize),
    rows: usize,
    columns: usize,
) -> kettle_render::MediaLaneSource {
    let total = source.text_rows();
    let (first_row, first_column) = scrolled_source(scroll, 0, 0, total);
    kettle_render::MediaLaneSource {
        rows: source.display_rows(first_row, rows, first_column, columns),
        first_row,
        total_rows: total,
    }
}

/// The worker's canvas for a lane's.
fn media_canvas(canvas: kettle_render::MediaCanvas) -> kettle_media::Canvas {
    match canvas {
        kettle_render::MediaCanvas::Theme => kettle_media::Canvas::Theme,
        kettle_render::MediaCanvas::White => kettle_media::Canvas::White,
        kettle_render::MediaCanvas::Checker => kettle_media::Canvas::Checker,
    }
}

/// The canvas a lane shows `item` on. The user's choice for a raster or an
/// SVG is drawn behind its pixels; a diagram's is the one its pixels were
/// rendered for, as its colors follow the canvas, until a render for the
/// choice comes back. Otherwise the kind's own: white for SVG, a checkerboard
/// for a raster's transparency, the pane's background for a diagram.
fn preview_canvas(
    item: &crate::media::ShelfItem,
    panel: &crate::window_state::PreviewPanel,
) -> kettle_render::MediaCanvas {
    use kettle_render::MediaCanvas as C;
    match item.kind {
        kind if crate::media::draws_as_diagram(kind) => match item.source.spec.canvas {
            kettle_media::Canvas::Theme => C::Theme,
            kettle_media::Canvas::White => C::White,
            kettle_media::Canvas::Checker => C::Checker,
        },
        crate::media::ItemKind::Media(kettle_media::MediaKind::Svg) => {
            panel.canvas.unwrap_or(C::White)
        }
        crate::media::ItemKind::Media(kettle_media::MediaKind::Raster) => {
            panel.canvas.unwrap_or(C::Checker)
        }
        _ => panel.canvas.unwrap_or(C::Theme),
    }
}

/// The canvas the user chose for `item`'s lane, or else the one it shows.
fn chosen_canvas(
    item: &crate::media::ShelfItem,
    panel: &crate::window_state::PreviewPanel,
) -> kettle_render::MediaCanvas {
    panel.canvas.unwrap_or_else(|| preview_canvas(item, panel))
}

/// What a lane says in place of its hint after `notice`.
fn lane_notice_text(
    notice: crate::window_state::LaneNotice,
    tr: &kettle_i18n::Translator,
) -> &'static str {
    use crate::window_state::LaneNotice as N;
    use kettle_i18n::Text as T;
    tr.text(match notice {
        N::ImageCopied => T::MediaLaneNoticeImageCopied,
        N::SourceCopied => T::MediaLaneNoticeSourceCopied,
        N::CopyFailed => T::MediaLaneNoticeCopyFailed,
        N::CopyBusy => T::MediaLaneNoticeCopyBusy,
        N::Rendering => T::MediaLaneNoticeRendering,
        N::RenderingPage => T::MediaLaneNoticeRenderingPage,
        N::RenderingDocumentPage => T::MediaLaneNoticeRenderingDocumentPage,
        N::RenderFailed => T::MediaLaneNoticeRenderFailed,
        N::Changed => T::MediaLaneNoticeChanged,
        N::PreviewFailed => T::MediaLaneNoticePreviewFailed,
        N::PreviewTimedOut => T::MediaLaneNoticePreviewTimedOut,
        N::PreviewNeedsDecoder => T::MediaLaneNoticePreviewNeedsDecoder,
        N::PreviewNeedsSandbox => T::MediaLaneNoticePreviewNeedsSandbox,
        N::VideoFailed => T::MediaLaneNoticeVideoFailed,
        N::VideoTimedOut => T::MediaLaneNoticeVideoTimedOut,
        N::VideoNeedsDecoder => T::MediaLaneNoticeVideoNeedsDecoder,
        N::VideoNeedsSandbox => T::MediaLaneNoticeVideoNeedsSandbox,
    })
}

/// What a screen reader hears that a lane last did, given its `preview` and
/// its `notice`: that a preview is being made, else the notice. A preview
/// that plays is said by its button's name, never frame by frame.
fn lane_announced(
    preview: Option<&crate::media::LanePreview>,
    notice: Option<crate::window_state::LaneNotice>,
    tr: &kettle_i18n::Translator,
) -> Option<String> {
    if preview.is_some_and(|preview| preview.waits_on().is_some()) {
        return Some(
            tr.text(kettle_i18n::Text::MediaLaneNoticePreviewLoading)
                .to_string(),
        );
    }
    notice.map(|notice| lane_notice_text(notice, tr).to_string())
}

/// Whether a container's video may go to Apple's decoder, the only one
/// that plays sound: MP4 and QuickTime on macOS, or a container not known
/// yet; elsewhere there is no Apple decoder to keep it from.
fn apple_reads(container: Option<kettle_media::video::VideoContainer>) -> bool {
    use kettle_media::video::VideoContainer as C;
    !cfg!(target_os = "macos")
        || container.is_none_or(|container| matches!(container, C::IsoBmff | C::QuickTime))
}

/// A playing video's sound in ctl `ui_geometry`: heard, muted, or why there
/// is none.
fn playback_sound_name(sound: crate::media::PlaybackSound) -> &'static str {
    use crate::media::PlaybackSound as S;
    use kettle_media::AudioStatus as A;
    match sound {
        S::Heard => "heard",
        S::Muted => "muted",
        S::Silent(A::NoTrack) => "no_track",
        S::Silent(A::DecoderSilent) => "decoder_silent",
        S::Silent(A::Unsupported) => "unsupported",
        S::Silent(A::Unavailable) => "unavailable",
        S::Silent(A::Off | A::Ready) => "off",
    }
}

/// What a lane's footer says of a video playing without sound, by why.
fn silence_text(why: kettle_media::AudioStatus) -> kettle_i18n::Text {
    use kettle_i18n::Text as T;
    use kettle_media::AudioStatus as A;
    match why {
        A::NoTrack => T::MediaLaneNoticeVideoNoSoundTrack,
        A::DecoderSilent => T::MediaLaneNoticeVideoSilentDecoder,
        A::Unsupported => T::MediaLaneNoticeVideoSoundUnsupported,
        A::Unavailable => T::MediaLaneNoticeVideoSoundUnavailable,
        // Sound that can play is heard or muted, never silent.
        A::Off | A::Ready => T::MediaLaneNoticeVideoPlaying,
    }
}

/// What a lane says when its video stops playing, or cannot open, with
/// `failure`: which decoder to install, that there is no sandbox, that the
/// file changed, that it took too long, or that it stopped.
fn playback_failure_notice(failure: kettle_media::FailureCode) -> crate::window_state::LaneNotice {
    use crate::window_state::LaneNotice as N;
    use kettle_media::FailureCode as F;
    match failure {
        F::UnsupportedContainer | F::CodecUnavailable | F::BackendUnavailable => {
            N::VideoNeedsDecoder
        }
        F::SandboxUnavailable => N::VideoNeedsSandbox,
        F::Changed => N::Changed,
        F::RenderTimeout => N::VideoTimedOut,
        _ => N::VideoFailed,
    }
}

/// What a lane says when its video's preview fails with `failure`: which
/// decoder to install, that there is no sandbox, that the file changed, or
/// that it could not be made.
fn preview_failure_notice(failure: kettle_media::FailureCode) -> crate::window_state::LaneNotice {
    use crate::window_state::LaneNotice as N;
    use kettle_media::FailureCode as F;
    match failure {
        F::UnsupportedContainer | F::CodecUnavailable | F::BackendUnavailable => {
            N::PreviewNeedsDecoder
        }
        F::SandboxUnavailable => N::PreviewNeedsSandbox,
        F::Changed => N::Changed,
        F::RenderTimeout => N::PreviewTimedOut,
        _ => N::PreviewFailed,
    }
}

/// What a screen reader hears for a preview lane: its name, with the item's
/// place on the shelf, title and detail line, and a description that says
/// what the lane last did (`announced`), else when the pixels were released
/// and where keys go.
fn media_lane_accessibility(
    lane: &kettle_render::MediaLanePanel,
    tr: &kettle_i18n::Translator,
    announced: Option<&str>,
) -> (String, String) {
    let (index, count) = lane.position;
    let detail = match lane.page.filter(|&(_, pages)| pages > 1) {
        Some((page, pages)) => format!(
            "{} · {} · {}",
            lane.detail,
            if lane.document {
                tr.media_lane_a11y_page(page as u64, pages as u64)
            } else {
                tr.media_lane_a11y_diagram(page as u64, pages as u64)
            },
            lane.sender.text
        ),
        None => format!("{} · {}", lane.detail, lane.sender.text),
    };
    let label = if lane.pin_label.is_some() {
        tr.media_viewer_a11y(index as u64, count as u64, &lane.title, &detail)
    } else {
        tr.media_lane_a11y(index as u64, count as u64, &lane.title, &detail)
    };
    let description = match announced {
        Some(announced) => announced.to_owned(),
        None if lane.image.is_some() || lane.frame.is_some() => lane.hint.clone(),
        None => format!("{} {}", lane.status, lane.hint),
    };
    (label, description)
}

/// Preview lanes and their controls, each by pane: the lane at part 0 and
/// its controls after it. Pane ids at or past [`MAX_LANE_ACCESSIBILITY_PANE`]
/// have no lane nodes rather than ones that could collide.
const ACCESSIBILITY_LANE_ID_MASK: u64 = 1 << 56;
const MAX_LANE_ACCESSIBILITY_PANE: u64 = 1 << 51;
const LANE_ACCESSIBILITY_PARTS: u64 = 32;
/// A lane's controls, at parts 1 and on (see [`lane_controls`]).
const LANE_CONTROLS: usize = 18;
/// A playing video's bar, a slider: the part after the controls', below
/// the stride.
const LANE_SCRUBBER_PART: u64 = 17;
const LANE_PIN_PART: u64 = LANE_SCRUBBER_PART + 1;
const _: () = assert!(LANE_PIN_PART < LANE_ACCESSIBILITY_PARTS);

/// How far a step along a playing video's bar moves it, from the keyboard
/// or an assistive technology.
const LANE_SEEK_STEP: std::time::Duration = std::time::Duration::from_secs(5);

/// How often a window whose lanes have video sessions comes round to keep
/// them: each session's worker ends it after five seconds without word, and
/// its client sends word only while the lane keeps it.
const LANE_PLAYBACK_KEEP: std::time::Duration = std::time::Duration::from_secs(1);

/// The node of `pane`'s lane (`part` 0) or one of its controls.
fn accessibility_lane_id(pane: u64, part: u64) -> Option<NodeId> {
    (pane < MAX_LANE_ACCESSIBILITY_PANE && part < LANE_ACCESSIBILITY_PARTS)
        .then(|| NodeId(ACCESSIBILITY_LANE_ID_MASK | (pane * LANE_ACCESSIBILITY_PARTS) | part))
}

/// The pane and part a lane node names, if it is a lane's.
fn accessibility_lane_part(id: NodeId) -> Option<(u64, u64)> {
    let payload = id.0 & (ACCESSIBILITY_LANE_ID_MASK - 1);
    (id.0 & !(ACCESSIBILITY_LANE_ID_MASK - 1) == ACCESSIBILITY_LANE_ID_MASK).then_some((
        payload / LANE_ACCESSIBILITY_PARTS,
        payload % LANE_ACCESSIBILITY_PARTS,
    ))
}

/// A lane's controls in order, with the part each answers to.
fn lane_controls(
    geometry: &kettle_render::MediaLaneGeometry,
) -> [(
    u64,
    Option<kettle_render::Rect4>,
    kettle_render::MediaLaneHit,
); LANE_CONTROLS] {
    use kettle_render::MediaLaneHit as Hit;
    [
        (1, geometry.previous, Hit::Previous),
        (2, geometry.next, Hit::Next),
        (3, geometry.open_outside, Hit::OpenOutside),
        (
            if geometry.pinning { LANE_PIN_PART } else { 4 },
            Some(geometry.toggle),
            if geometry.pinning {
                Hit::Pin
            } else {
                Hit::Toggle
            },
        ),
        (5, Some(geometry.close), Hit::Close),
        (6, geometry.mode, Hit::Mode),
        (7, geometry.canvas, Hit::Canvas),
        (8, geometry.copy, Hit::Copy),
        (9, geometry.reload, Hit::Reload),
        (10, geometry.zoom_out, Hit::ZoomOut),
        (11, geometry.zoom_in, Hit::ZoomIn),
        (12, geometry.zoom_fit, Hit::Fit),
        (13, geometry.page_previous, Hit::PreviousPage),
        (14, geometry.page_next, Hit::NextPage),
        (15, geometry.play, Hit::Play),
        (16, geometry.sound, Hit::Sound),
        (19, geometry.theater, Hit::Theater),
        (20, geometry.fullscreen, Hit::Fullscreen),
    ]
}

fn lane_interactive_controls(
    lane: &kettle_render::MediaLanePanel,
    geometry: &kettle_render::MediaLaneGeometry,
) -> Vec<(u64, Rect, kettle_render::MediaLaneHit)> {
    lane_controls(geometry)
        .into_iter()
        .chain(geometry.scrubber.map(|rect| {
            (
                LANE_SCRUBBER_PART,
                Some(rect),
                kettle_render::MediaLaneHit::Scrub,
            )
        }))
        .filter_map(|(part, rect, hit)| Some((part, rect?, hit)))
        .filter(|(_, _, hit)| lane_control_enabled(lane, *hit))
        .collect()
}

fn accessibility_lane_controls_key(
    lane: &kettle_render::MediaLanePanel,
    geometry: Option<&kettle_render::MediaLaneGeometry>,
) -> u64 {
    use std::hash::{Hash as _, Hasher as _};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (lane.mode == kettle_render::MediaLaneMode::Source).hash(&mut hasher);
    lane.pin_label.is_some().hash(&mut hasher);
    geometry.is_some().hash(&mut hasher);
    let rect_key =
        |(x, y, width, height): kettle_render::Rect4| [x, y, width, height].map(f32::to_bits);
    if let Some(geometry) = geometry {
        for (part, rect, hit) in lane_controls(geometry) {
            (part, rect.map(rect_key), lane_control_enabled(lane, hit)).hash(&mut hasher);
        }
        geometry.scrubber.map(rect_key).hash(&mut hasher);
        if geometry.scrubber.is_some() {
            lane.scrubber
                .as_ref()
                .map(|bar| (bar.position_ms, bar.duration_ms))
                .hash(&mut hasher);
        }
    }
    hasher.finish()
}

fn lane_control_enabled(
    lane: &kettle_render::MediaLanePanel,
    hit: kettle_render::MediaLaneHit,
) -> bool {
    use kettle_render::MediaLaneHit as Hit;
    match hit {
        Hit::Previous => lane.position.0 > 1,
        Hit::Next => lane.position.0 < lane.position.1,
        Hit::PreviousPage => lane.page.is_some_and(|(page, _)| page > 1),
        Hit::NextPage => lane.page.is_some_and(|(page, count)| page < count),
        _ => true,
    }
}

/// A lane control's name in ctl `ui_geometry`.
fn lane_control_name(hit: kettle_render::MediaLaneHit) -> &'static str {
    use kettle_render::MediaLaneHit as Hit;
    match hit {
        Hit::Previous => "previous",
        Hit::Next => "next",
        Hit::PreviousPage => "previous_page",
        Hit::NextPage => "next_page",
        Hit::OpenOutside => "open_outside",
        Hit::Toggle => "toggle",
        Hit::Pin => "pin",
        Hit::Close => "close",
        Hit::Mode => "mode",
        Hit::Canvas => "canvas",
        Hit::Copy => "copy",
        Hit::Reload => "reload",
        Hit::Play => "play",
        Hit::Sound => "sound",
        Hit::Theater => "theater",
        Hit::Fullscreen => "fullscreen",
        Hit::Scrub => "scrubber",
        Hit::ZoomOut => "zoom_out",
        Hit::ZoomIn => "zoom_in",
        Hit::Fit => "fit",
        Hit::Resize => "resize",
        Hit::Inside => "inside",
    }
}

fn accessibility_card_id(instance: u64) -> NodeId {
    NodeId(ACCESSIBILITY_CARD_ID_MASK | instance)
}

/// The card instance a node names, if it is a card's.
fn accessibility_card_instance(id: NodeId) -> Option<u64> {
    let instance = id.0 & (ACCESSIBILITY_CARD_ID_MASK - 1);
    (id.0 & !(ACCESSIBILITY_CARD_ID_MASK - 1) == ACCESSIBILITY_CARD_ID_MASK && instance != 0)
        .then_some(instance)
}

/// `text` without placeholder sequences. A label built from outside text,
/// such as a sender's path, reaches a screen reader or a control client and
/// must never carry a card's marks.
fn without_card_marks(text: String) -> String {
    match kettle_core::strip_placeholders(&text) {
        std::borrow::Cow::Borrowed(_) => text,
        std::borrow::Cow::Owned(stripped) => stripped,
    }
}

/// A card's button for assistive technology: named by its item's title and
/// the viewer's detail line, never by its nonce, at the part of the card on
/// screen. Clicking it opens the item in the viewer.
fn card_accessibility_node(
    rect: [f32; 4],
    title: &str,
    detail: &str,
    tr: &kettle_i18n::Translator,
) -> Node {
    let mut node = Node::new(Role::Button);
    node.set_label(without_card_marks(tr.card_a11y(title, detail)));
    node.set_description(tr.text(kettle_i18n::Text::CardA11yOpen));
    node.add_action(AccessibilityAction::Focus);
    node.add_action(AccessibilityAction::Click);
    let [x, y, width, height] = rect;
    node.set_bounds(accesskit::Rect::new(
        f64::from(x),
        f64::from(y),
        f64::from(x + width),
        f64::from(y + height),
    ));
    node
}

fn accessibility_modal_id(kind: u64, index: usize) -> NodeId {
    NodeId(ACCESSIBILITY_MODAL_ID_MASK | (kind << 16) | index as u64)
}

fn accessibility_confirm_button_index(id: NodeId) -> Option<usize> {
    let first = accessibility_modal_id(1, 1).0;
    let raw = id.0.checked_sub(first)?;
    (raw < (1 << 16) - 1).then_some(raw as usize)
}

fn accessibility_context_menu_item_index(id: NodeId, ws: &WindowState) -> Option<usize> {
    if ws.confirm_dialog.is_some()
        || ws.palette_input.is_some()
        || ws.theme_picker_input.is_some()
        || ws.layout_picker_input.is_some()
        || ws.ssh_input.is_some()
    {
        return None;
    }
    let raw = id.0.checked_sub(accessibility_modal_id(2, 1).0)?;
    if raw >= (1 << 16) - 1 {
        return None;
    }
    let index = raw as usize;
    ws.context_menu
        .as_ref()?
        .items
        .get(index)
        .is_some_and(item_is_dispatchable)
        .then_some(index)
}

struct ModalAccessibilityProjection {
    roots: Vec<NodeId>,
    nodes: Vec<(NodeId, Node)>,
    focus: Option<NodeId>,
}

fn push_picker_accessibility(
    projection: &mut ModalAccessibilityProjection,
    menu: Option<&ContextMenu>,
    kind: u64,
    label: &str,
    query: &str,
    full_bounds: accesskit::Rect,
    tr: &kettle_i18n::Translator,
) {
    let container_id = accessibility_modal_id(kind, 0);
    let input_id = accessibility_modal_id(kind, 1);
    let list_id = accessibility_modal_id(kind, 2);
    let mut input = Node::new(Role::TextInput);
    input.set_label(label);
    input.set_value(query);
    projection.nodes.push((input_id, input));

    let mut list_children = Vec::new();
    let mut selected = None;
    if let Some(menu) = menu {
        for (index, row) in menu.rows.iter().enumerate() {
            if row.separator {
                continue;
            }
            let id = accessibility_modal_id(kind, index + 3);
            let mut option = Node::new(Role::ListBoxOption);
            option.set_label(row.label.clone());
            // The painted hint, such as a shortcut or a theme's appearance.
            if !row.hint.is_empty() {
                option.set_description(row.hint.clone());
            }
            if !row.enabled {
                option.set_disabled();
            }
            option.set_selected(index == menu.highlight);
            if index == menu.highlight {
                selected = Some(id);
            }
            list_children.push(id);
            projection.nodes.push((id, option));
        }
    }
    let mut list = Node::new(Role::ListBox);
    list.set_label(tr.picker_a11y_results(label));
    list.set_children(list_children);
    projection.nodes.push((list_id, list));

    let mut dialog = Node::new(Role::Dialog);
    dialog.set_label(label);
    dialog.set_children([input_id, list_id]);
    dialog.set_bounds(full_bounds);
    projection.roots.push(container_id);
    projection.nodes.push((container_id, dialog));
    if projection.focus.is_none() {
        projection.focus = selected.or(Some(input_id));
    }
}

fn modal_accessibility_projection(
    overlay: &Overlay,
    surface: (f32, f32),
    metrics: (f32, f32),
    scale_factor: f32,
    tr: &kettle_i18n::Translator,
) -> ModalAccessibilityProjection {
    let full_bounds = accesskit::Rect::new(0.0, 0.0, f64::from(surface.0), f64::from(surface.1));
    let mut projection = ModalAccessibilityProjection {
        roots: Vec::new(),
        nodes: Vec::new(),
        focus: None,
    };

    if let Some(dialog) = overlay.confirm_dialog.as_ref() {
        let layout =
            kettle_render::ConfirmDialogLayout::new(surface, metrics, scale_factor, dialog);
        let bounds = |(x, y, w, h): (f32, f32, f32, f32)| {
            accesskit::Rect::new(x as f64, y as f64, (x + w) as f64, (y + h) as f64)
        };
        let container_id = accessibility_modal_id(1, 0);
        let mut children = Vec::with_capacity(dialog.buttons.len());
        for (index, button) in dialog.buttons.iter().enumerate() {
            let id = accessibility_modal_id(1, index + 1);
            let mut node = Node::new(Role::Button);
            node.set_label(button.label.clone());
            if let Some(rect) = layout.buttons.get(index) {
                node.set_bounds(bounds(*rect));
            }
            node.add_action(AccessibilityAction::Focus);
            node.add_action(AccessibilityAction::Click);
            children.push(id);
            projection.nodes.push((id, node));
        }
        let mut node = Node::new(Role::AlertDialog);
        node.set_label(dialog.heading.clone());
        node.set_description(dialog.body.clone());
        node.set_modal();
        if layout.max_scroll > 0 {
            node.add_action(AccessibilityAction::ScrollUp);
            node.add_action(AccessibilityAction::ScrollDown);
        }
        node.set_children(children.clone());
        node.set_bounds(bounds(layout.rect));
        projection.roots.push(container_id);
        projection.nodes.push((container_id, node));
        projection.focus = children
            .get(dialog.focus_idx.min(children.len().saturating_sub(1)))
            .copied()
            .or(Some(container_id));
        return projection;
    }

    let picker_open = overlay.palette_query.is_some()
        || overlay.theme_picker_query.is_some()
        || overlay.layout_picker_query.is_some()
        || overlay.ssh_query.is_some();
    if !picker_open && let Some(menu) = overlay.context_menu.as_ref() {
        let container_id = accessibility_modal_id(2, 0);
        let mut children = Vec::new();
        let mut highlighted = None;
        for (index, row) in menu.rows.iter().enumerate() {
            if row.separator {
                continue;
            }
            let id = accessibility_modal_id(2, index + 1);
            let mut node = Node::new(Role::MenuItem);
            node.set_label(row.label.clone());
            if !row.hint.is_empty() {
                node.set_description(row.hint.clone());
            }
            if !row.enabled {
                node.set_disabled();
            } else {
                node.add_action(AccessibilityAction::Focus);
                node.add_action(AccessibilityAction::Click);
            }
            if index == menu.highlight {
                highlighted = Some(id);
            }
            children.push(id);
            projection.nodes.push((id, node));
        }
        let mut node = Node::new(Role::Menu);
        node.set_label(
            menu.a11y_label
                .clone()
                .unwrap_or_else(|| tr.text(kettle_i18n::Text::MenuA11yTerminal).to_string()),
        );
        node.set_children(children);
        node.set_bounds(full_bounds);
        projection.roots.push(container_id);
        projection.nodes.push((container_id, node));
        if projection.focus.is_none() {
            projection.focus = highlighted.or(Some(container_id));
        }
    }

    if !overlay.hint_labels.is_empty() {
        let container_id = accessibility_modal_id(3, 0);
        let mut children = Vec::with_capacity(overlay.hint_labels.len());
        for (index, hint) in overlay.hint_labels.iter().enumerate() {
            let id = accessibility_modal_id(3, index + 1);
            let mut node = Node::new(Role::ListBoxOption);
            node.set_label(hint.label.clone());
            if hint.dim {
                node.set_disabled();
            }
            children.push(id);
            projection.nodes.push((id, node));
        }
        let mut node = Node::new(Role::ListBox);
        node.set_label(tr.text(kettle_i18n::Text::HintA11yList));
        node.set_children(children);
        projection.roots.push(container_id);
        projection.nodes.push((container_id, node));
        if projection.focus.is_none() {
            projection.focus = Some(container_id);
        }
    }

    if let Some(query) = overlay.palette_query.as_deref() {
        push_picker_accessibility(
            &mut projection,
            overlay.context_menu.as_ref(),
            4,
            tr.text(kettle_i18n::Text::PickerA11yPalette),
            query,
            full_bounds,
            tr,
        );
    }
    if let Some(query) = overlay.theme_picker_query.as_deref() {
        push_picker_accessibility(
            &mut projection,
            overlay.context_menu.as_ref(),
            10,
            tr.text(kettle_i18n::Text::PickerA11yThemes),
            query,
            full_bounds,
            tr,
        );
    }

    if let Some(settings) = overlay.settings.as_ref() {
        let container_id = accessibility_modal_id(5, 0);
        let list_id = accessibility_modal_id(5, 1);
        let mut children = Vec::with_capacity(settings.rows.len());
        for (index, row) in settings.rows.iter().enumerate() {
            let id = accessibility_modal_id(5, index + 2);
            let mut node = Node::new(Role::ListBoxOption);
            node.set_label(row.label.clone());
            node.set_value(row.value.clone());
            if row.disabled {
                node.set_disabled();
            }
            node.set_selected(index == settings.focused_row);
            children.push(id);
            projection.nodes.push((id, node));
        }
        let mut list = Node::new(Role::ListBox);
        list.set_label(tr.text(kettle_i18n::Text::SettingsA11yFields));
        list.set_children(children.clone());
        projection.nodes.push((list_id, list));
        let mut dialog = Node::new(Role::Dialog);
        dialog.set_label(match settings.categories.get(settings.active_category) {
            Some(category) => tr.settings_a11y_dialog(category),
            None => tr.text(kettle_i18n::Text::SettingsA11yTitle).to_string(),
        });
        dialog.set_children([list_id]);
        dialog.set_bounds(full_bounds);
        projection.roots.push(container_id);
        projection.nodes.push((container_id, dialog));
        if projection.focus.is_none() {
            projection.focus = children
                .get(settings.focused_row.min(children.len().saturating_sub(1)))
                .copied()
                .or(Some(container_id));
        }
    }

    if let Some(query) = overlay.layout_picker_query.as_deref() {
        push_picker_accessibility(
            &mut projection,
            overlay.context_menu.as_ref(),
            6,
            tr.text(kettle_i18n::Text::PickerA11yLayouts),
            query,
            full_bounds,
            tr,
        );
    }
    if let Some(query) = overlay.ssh_query.as_deref() {
        push_picker_accessibility(
            &mut projection,
            overlay.context_menu.as_ref(),
            7,
            tr.text(kettle_i18n::Text::PickerA11ySsh),
            query,
            full_bounds,
            tr,
        );
    }

    if let Some(edit) = overlay.edit_title.as_ref() {
        let container_id = accessibility_modal_id(8, 0);
        let input_id = accessibility_modal_id(8, 1);
        let mut input = Node::new(Role::TextInput);
        input.set_label(edit.label.clone());
        input.set_value(edit.input.clone());
        projection.nodes.push((input_id, input));
        let mut dialog = Node::new(Role::Dialog);
        dialog.set_label(edit.label.clone());
        dialog.set_children([input_id]);
        dialog.set_bounds(full_bounds);
        projection.roots.push(container_id);
        projection.nodes.push((container_id, dialog));
        if projection.focus.is_none() {
            projection.focus = Some(input_id);
        }
    }

    projection
}

#[derive(Clone)]
struct AccessibilityActivation {
    initial: TreeUpdate,
    proxy: EventLoopProxy<UserEvent>,
    window_seq: u64,
}

impl ActivationHandler for AccessibilityActivation {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        let _ = self.proxy.send_event(UserEvent::AccessibilityActivated {
            window_seq: self.window_seq,
        });
        Some(self.initial.clone())
    }
}

struct AccessibilityActions {
    proxy: EventLoopProxy<UserEvent>,
    window_seq: u64,
}

impl ActionHandler for AccessibilityActions {
    fn do_action(&mut self, request: ActionRequest) {
        let _ = self.proxy.send_event(UserEvent::AccessibilityAction {
            window_seq: self.window_seq,
            request,
        });
    }
}

struct AccessibilityDeactivation;

impl DeactivationHandler for AccessibilityDeactivation {
    fn deactivate_accessibility(&mut self) {}
}

/// Live state for a split-divider mouse drag — the addressed
/// split (`path`) and its orientation. The split's rect is re-fetched from
/// `split_seams` on every move (keyed by `path`) so a layout change mid-drag
/// (another pane closing) can't desync the ratio math.
pub(crate) struct SplitDrag {
    path: Vec<bool>,
    dir: Dir,
}

/// Terminator parity: live state for dragging a terminal to another position
/// inside its tab, armed by a left-press on that pane's own titlebar.
///
/// The press is deliberately ambiguous until the pointer moves. A titlebar
/// click means "focus this pane" and a second click on the focused pane means
/// "edit its title"; both must keep working. So the press records what it
/// *would* do, `live` records whether movement has since turned it into a drag,
/// and the RELEASE picks between the two. Opening the title editor on press
/// would put an editor over the pane the user just started dragging.
pub(crate) struct PaneDrag {
    /// The pane being moved.
    pub(crate) pane: u64,
    /// Surface position of the press, the origin the slop radius is measured
    /// from.
    pub(crate) origin: (f32, f32),
    /// Movement has cleared the slop radius: this is a drag, not a click.
    pub(crate) live: bool,
    /// A release that never became a drag opens the title editor. True only
    /// when the press landed on the ALREADY-focused pane: a press that merely
    /// moved focus is not by itself a request to rename anything.
    pub(crate) rename_on_click: bool,
    /// Latched drop target while `live`: the pane under the cursor, the half of
    /// it the drop would take, and the `(dir, before)` pair that describes that
    /// half to `Mux::move_pane_beside`. `None` while the cursor is over no pane
    /// — over a seam, or outside the content area.
    pub(crate) target: Option<(u64, Rect, Dir, bool)>,
}

/// An in-flight `run_command` awaiting its OSC-133
/// completion. The control server wrote `cmd\r` to the pane; the matching
/// `CommandFinished` generation resolves the request with the exit code,
/// duration, and the output captured since `start_line`. A deadline guards the
/// no-shell-integration case (the command runs but no `CommandEnd` ever fires).
struct PendingRun {
    /// Connection that issued the run (for disconnect cleanup).
    conn_id: u64,
    /// Request id, echoed in the completion response.
    req_id: u64,
    /// Absolute line, counted from the first row ever written (evicted history
    /// included), where the command's output begins, so the reply can slice
    /// just this command's output out of the grid even after the ring wraps.
    start_line: u64,
    /// Command-start generation observed before submitting input.
    command_generation: u64,
    /// When the request gives up waiting for OSC-133 and replies `timed_out`.
    deadline: std::time::Instant,
    /// Where the completion response is sent (the connection thread is blocked
    /// reading this until the command finishes or the deadline fires).
    reply: crate::ctl_server::ReplyTx,
}

fn take_pending_run(
    runs: &mut std::collections::HashMap<u64, PendingRun>,
    pane: u64,
    event: &kettle_core::CommandFinished,
) -> Option<PendingRun> {
    if event.generation != runs.get(&pane)?.command_generation.wrapping_add(1) {
        return None;
    }
    runs.remove(&pane)
}

struct GpuRecoveryOutcome {
    adapter: kettle_render::GpuAdapterInfo,
    secondary_window_failures: usize,
}

type RecoveryBuilds<T> = Vec<(u64, T)>;
type RecoveryBuildErrors<E> = Vec<(u64, E)>;

fn build_recovery_set<T, O, E>(
    targets: impl IntoIterator<Item = (u64, T)>,
    mut build: impl FnMut(T) -> std::result::Result<O, E>,
) -> std::result::Result<RecoveryBuilds<O>, RecoveryBuildErrors<E>> {
    let mut built = Vec::new();
    let mut errors = Vec::new();
    for (seq, target) in targets {
        match build(target) {
            Ok(output) => built.push((seq, output)),
            Err(error) => errors.push((seq, error)),
        }
    }
    if errors.is_empty() {
        Ok(built)
    } else {
        Err(errors)
    }
}

/// Grab tolerance (px) for the thin split divider line, so
/// it's easy to hit with the mouse without pixel-perfect aim.
const SPLIT_SEAM_TOL: f32 = 5.0;
const CELL_FLAG_UNDERLINE: u16 = 0b0000_0000_0000_1000;
const CELL_FLAG_STRIKEOUT: u16 = 0b0000_0010_0000_0000;
const CELL_FLAG_DOUBLE_UNDERLINE: u16 = 0b0000_1000_0000_0000;
const CELL_FLAG_UNDERCURL: u16 = 0b0001_0000_0000_0000;
const CELL_FLAG_DOTTED_UNDERLINE: u16 = 0b0010_0000_0000_0000;
const CELL_FLAG_DASHED_UNDERLINE: u16 = 0b0100_0000_0000_0000;
const CELL_FLAG_ALL_UNDERLINES: u16 = CELL_FLAG_UNDERLINE
    | CELL_FLAG_DOUBLE_UNDERLINE
    | CELL_FLAG_UNDERCURL
    | CELL_FLAG_DOTTED_UNDERLINE
    | CELL_FLAG_DASHED_UNDERLINE;
const MAX_CTL_SELECTION_BYTES: usize = 128 * 1024;
const MAX_CTL_TEXT_PAGE_BYTES: usize = 256 * 1024;
const MAX_CTL_COMMAND_OUTPUT_BYTES: usize = 512 * 1024;
/// Bound live-state enumeration before building JSON values or terminal
/// snapshots. This matches the core search materialization ceiling and is far
/// above a plausible interactive terminal/window inventory.
const MAX_CTL_COLLECTION_ITEMS: usize = 262_144;
/// `read_screen` pages are smaller than this, but the complete snapshot is
/// hashed for stable pagination. Reject before materializing a larger scrape.
const MAX_CTL_SCREEN_SNAPSHOT_BYTES: usize = 512 * 1024;
const MAX_CTL_SCROLLBACK_LINES: usize = 10_000;
const MAX_CTL_SEND_KEYS: usize = 1024;
const MAX_CTL_SEND_KEY_TOKEN_BYTES: usize = 64;
const MAX_CTL_SEND_KEY_BYTES: usize = 64 * 1024;

fn optional_u64_param(
    params: &serde_json::Value,
    name: &str,
) -> std::result::Result<Option<u64>, String> {
    match params.get(name) {
        None => Ok(None),
        Some(value) => value
            .as_u64()
            .map(Some)
            .ok_or_else(|| format!("'{name}' must be an unsigned integer")),
    }
}

fn ctl_capture_bounds(
    history_origin: u64,
    history_size: usize,
    start_line: u64,
    cursor_abs: u64,
    returned_lines: usize,
) -> (usize, usize, bool) {
    let screen_top = history_origin.saturating_add(history_size as u64);
    let take = screen_top
        .saturating_sub(start_line)
        .min(MAX_CTL_SCROLLBACK_LINES as u64)
        .max(1)
        .min(history_size as u64);
    let first_abs = screen_top.saturating_sub(take);
    let start_idx = start_line
        .saturating_sub(first_abs)
        .min(returned_lines as u64) as usize;
    // Inclusive of the cursor line; clamp to what screen_text actually returned.
    let end_idx = cursor_abs
        .saturating_sub(first_abs)
        .saturating_add(1)
        .min(returned_lines as u64) as usize;
    (start_idx, end_idx, start_line < first_abs)
}

/// This file and the production modules in `app/`, as one text, for the
/// guards that read the App's source. A method moved to a module is
/// `pub(super)` only so app.rs can still call it; the guards read it as the
/// private method it is.
#[cfg(test)]
pub(crate) fn app_source() -> String {
    [
        include_str!("app.rs"),
        include_str!("app/ctl.rs"),
        include_str!("app/automation.rs"),
        include_str!("app/media.rs"),
        include_str!("app/preview.rs"),
        include_str!("app/paste.rs"),
        include_str!("app/search.rs"),
        include_str!("app/menus.rs"),
        include_str!("app/modals.rs"),
        include_str!("app/windowing.rs"),
        include_str!("app/accessibility.rs"),
        include_str!("app/config_reload.rs"),
        include_str!("app/pointer.rs"),
        include_str!("app/chrome.rs"),
        include_str!("app/gpu_recovery.rs"),
        include_str!("app/sessions.rs"),
        include_str!("app/system_opener.rs"),
    ]
    .concat()
    .replace("\n    pub(super) fn ", "\n    fn ")
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

#[cfg(any(test, target_os = "macos"))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MacosTitlebarPolicy {
    /// Keep AppKit's geometry and controls on an opaque native surface whose
    /// light or dark appearance follows Kettle's theme hint.
    ThemeMatchedNative,
    /// Borderless windows have no native titlebar to configure.
    NotApplicable,
}

#[cfg(any(test, target_os = "macos"))]
fn macos_titlebar_policy(borderless: bool) -> MacosTitlebarPolicy {
    if borderless {
        MacosTitlebarPolicy::NotApplicable
    } else {
        MacosTitlebarPolicy::ThemeMatchedNative
    }
}

#[cfg(target_os = "macos")]
fn apply_macos_window_chrome(
    attrs: winit::window::WindowAttributes,
    borderless: bool,
) -> winit::window::WindowAttributes {
    use winit::platform::macos::WindowAttributesExtMacOS as _;

    match macos_titlebar_policy(borderless) {
        // Create the caption opaque from the first frame. The material effect
        // stays inside the content rect below it.
        MacosTitlebarPolicy::ThemeMatchedNative => attrs.with_titlebar_transparent(false),
        MacosTitlebarPolicy::NotApplicable => attrs.with_decorations(false),
    }
}

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

fn cap_ctl_selection(mut text: String) -> (String, bool) {
    if text.len() <= MAX_CTL_SELECTION_BYTES {
        return (text, false);
    }
    let mut end = MAX_CTL_SELECTION_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    (text, true)
}

fn cap_ctl_command_response(
    mut response: kettle_ctl::protocol::Response,
) -> kettle_ctl::protocol::Response {
    use kettle_ctl::protocol::{MAX_RESPONSE_LINE_BYTES, to_json_vec_bounded};
    let serde_json::Value::String(mut output) = response.result["output"].take() else {
        return response;
    };
    response.result["output"] = serde_json::json!("");
    let overhead = to_json_vec_bounded(&response, MAX_RESPONSE_LINE_BYTES)
        .map_or(MAX_RESPONSE_LINE_BYTES, |bytes| bytes.len());
    let budget = MAX_RESPONSE_LINE_BYTES.saturating_sub(overhead);
    let mut encoded = 0;
    let mut end = 0;
    for (at, ch) in output.char_indices() {
        let cost = match ch {
            '"' | '\\' | '\u{8}' | '\t' | '\n' | '\u{c}' | '\r' => 2,
            '\0'..='\u{1f}' => 6,
            _ => ch.len_utf8(),
        };
        if at + ch.len_utf8() > MAX_CTL_COMMAND_OUTPUT_BYTES || encoded + cost > budget {
            break;
        }
        encoded += cost;
        end = at + ch.len_utf8();
    }
    if end < output.len() {
        output.truncate(end);
        response.result["output_truncated"] = serde_json::json!(true);
    }
    response.result["output"] = serde_json::Value::String(output);
    response
}

/// Merge independently-owned placement classes without letting a saturated
/// class hide every item from the others. This preserves all three bounded
/// registries for visibility culling; the renderer applies the smaller shared
/// frame quota after removing offscreen placements.
fn round_robin_bounded<T, const N: usize>(groups: [Vec<T>; N], limit: usize) -> Vec<T> {
    let mut groups = groups.map(Vec::into_iter);
    let mut out = Vec::with_capacity(
        groups
            .iter()
            .map(|group| group.len())
            .sum::<usize>()
            .min(limit),
    );
    while out.len() < limit {
        let before = out.len();
        for group in &mut groups {
            if let Some(item) = group.next() {
                out.push(item);
                if out.len() == limit {
                    break;
                }
            }
        }
        if out.len() == before {
            break;
        }
    }
    out
}

fn ctl_snapshot(kind: &str, value: &(impl serde::Serialize + ?Sized)) -> String {
    // Stable FNV-1a is sufficient here: the token detects live-state drift; it
    // is not an authentication primitive.
    struct Hasher(u64);
    impl std::io::Write for Hasher {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            for &byte in bytes {
                self.0 ^= u64::from(byte);
                self.0 = self.0.wrapping_mul(0x100000001b3);
            }
            Ok(bytes.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut hasher = Hasher(0xcbf29ce484222325);
    let _ = std::io::Write::write_all(&mut hasher, kind.as_bytes());
    let _ = serde_json::to_writer(&mut hasher, value);
    format!("{:016x}", hasher.0)
}

fn media_receipt_accessibility_label(
    kind: &kettle_render::MediaPasteReceiptKind,
    remote: bool,
    tr: &kettle_i18n::Translator,
) -> String {
    let mut label = match kind {
        kettle_render::MediaPasteReceiptKind::Image {
            original_width,
            original_height,
        } => tr.receipt_a11y_image(u64::from(*original_width), u64::from(*original_height)),
        kettle_render::MediaPasteReceiptKind::Video {
            extension,
            size,
            count,
            ..
        } if *count > 1 => tr.receipt_a11y_videos(
            *count as u64,
            extension,
            &kettle_render::format_media_size(*size),
        ),
        kettle_render::MediaPasteReceiptKind::Video {
            extension, size, ..
        } => tr.receipt_a11y_video(extension, &kettle_render::format_media_size(*size)),
    };
    if remote {
        label.push_str(", ");
        label.push_str(tr.text(kettle_i18n::Text::ReceiptA11yRemote));
    }
    label
}

/// Sum a lazily-produced inventory while stopping as soon as the budget is
/// crossed. Keeping this lazy is important: the limit exists to bound the
/// enumeration itself, not merely the result allocation that follows it.
fn ctl_bounded_sum(counts: impl IntoIterator<Item = usize>, limit: usize) -> Option<usize> {
    let mut total = 0usize;
    for count in counts {
        total = total.checked_add(count)?;
        if total > limit {
            return None;
        }
    }
    Some(total)
}

#[cfg(test)]
fn ctl_page_values(
    req: &kettle_ctl::protocol::Request,
    key: &str,
    values: Vec<serde_json::Value>,
) -> kettle_ctl::protocol::Response {
    use kettle_ctl::protocol::{PageRequest, Response, error_codes as ec};

    let page = match PageRequest::from_params(&req.params) {
        Ok(page) => page,
        Err(message) => return Response::err(req.id, ec::BAD_PARAMS, message),
    };
    ctl_page_values_with_page(req, key, values, page)
}

/// `get_state`'s `media` field: whether media previews are available, and a
/// fixed reason code when they are not. Asking never waits on a check.
/// Why `show` cannot render now, if it cannot: no worker, or one that
/// failed its checks. A worker still being checked is waited for.
fn media_unavailable(
    media: Option<&kettle_media::client::WorkerClient>,
) -> Option<kettle_media::FailureCode> {
    use kettle_media::FailureCode;
    use kettle_media::client::{MediaAvailability, UnavailableCause};
    match media.map(kettle_media::client::WorkerClient::availability) {
        None => Some(FailureCode::WorkerUnavailable),
        Some(MediaAvailability::Checking | MediaAvailability::Available) => None,
        Some(MediaAvailability::Unavailable(UnavailableCause::UnsupportedPlatform)) => {
            Some(FailureCode::UnsupportedPlatform)
        }
        Some(MediaAvailability::Unavailable(_)) => Some(FailureCode::WorkerUnavailable),
    }
}

/// The shelf items on screen in any window, whose pixels eviction spares:
/// each preview lane's, and each item whose card the last frame drew.
fn media_visible_items<'a>(
    windows: impl Iterator<Item = &'a WindowState>,
    cards: &crate::media::CardLedger,
) -> Vec<u64> {
    let mut visible = Vec::new();
    for window in windows {
        visible.extend(window.preview_panels.values().map(|panel| panel.item));
        if let Some(renderer) = &window.renderer {
            visible.extend(
                renderer
                    .drawn_cards()
                    .iter()
                    .filter_map(|card| cards.item(card.pane, card.nonce)),
            );
        }
    }
    visible
}

/// Whether the release of a press card `pressed` took opens it: only over the
/// very card it pressed, and only while nothing that owns the pointer (a
/// dialog, a menu, the viewer) has opened over it since the press.
fn card_release_opens(
    pressed: (u64, kettle_core::InlineNonce),
    over: Option<kettle_render::PaintedCard>,
    covered: bool,
) -> bool {
    !covered && over.is_some_and(|card| (card.pane, card.nonce) == pressed)
}

/// What an item is, in a lane's detail line or a card's screen-reader label:
/// its kind and size, and for a video its own size (not its poster's),
/// codec, length and whether it has sound.
fn media_item_detail(
    tr: kettle_i18n::Translator,
    kind: &str,
    item: &crate::media::ShelfItem,
) -> String {
    if let Some(facts) = item.kind.file() {
        return format!("{kind} · {}", kettle_ctl::show::size_label(facts.bytes));
    }
    let Some(video) = &item.video else {
        let (width, height) = item.size;
        return format!("{kind} · {width}×{height}");
    };
    let mut detail = format!("{kind} · {}×{}", video.width, video.height);
    if let Some(codec) = video.codec.name() {
        detail.push_str(" · ");
        detail.push_str(codec);
    }
    detail.push_str(" · ");
    detail.push_str(&kettle_render::duration_label(video.duration_ms));
    detail.push_str(" · ");
    detail.push_str(tr.text(if video.has_audio {
        kettle_i18n::Text::MediaVideoWithAudio
    } else {
        kettle_i18n::Text::MediaVideoNoAudio
    }));
    detail
}

/// Register an inline card for a published item and build the message its
/// harness prints, or `kettle show --inline` prints itself. Refused when the
/// owner is over its limits, the pane cannot hold a card, no nonce could be
/// had, or a harness's message would not print whole: the item stays on the
/// shelf either way.
#[allow(clippy::too_many_arguments)]
fn register_card(
    cards: &mut crate::media::CardLedger,
    (pane_id, pane_cards, grid, column): (
        u64,
        &mut kettle_render::InlineCards,
        (usize, usize),
        usize,
    ),
    cell: (f32, f32),
    item: (u64, u64),
    (kind, video, page): (
        crate::media::ItemKind,
        Option<&kettle_media::VideoInfo>,
        Option<kettle_media::PageOf>,
    ),
    image: &kettle_core::ImageData,
    inline: crate::media::InlineDraft,
    now: std::time::Instant,
) -> Result<kettle_ctl::show::InlineDelivery, kettle_ctl::show::InlineRefusal> {
    use kettle_ctl::show::InlineRefusal;
    cards
        .admit(inline.owner, now)
        .map_err(|_| InlineRefusal::Limit)?;
    let size = crate::media::card_size(inline.harness, grid, cell, (image.width, image.height))
        .ok_or(InlineRefusal::TooSmall)?;
    let nonce = cards
        .mint(|pool| getrandom::fill(pool).is_ok())
        .ok_or(InlineRefusal::Limit)?;
    let room = grid.0.saturating_sub(inline.harness.left() + 1);
    let caption = crate::media::card_caption(
        inline.name.as_deref(),
        kind,
        video.map_or((image.width, image.height), |video| {
            (video.width, video.height)
        }),
        video.map(|video| video.duration_ms),
        page,
        room,
    );
    let cli = inline.harness == kettle_render::CardHarness::Cli;
    let message = if cli {
        kettle_render::cli_card_text(nonce, size, &caption, column)
    } else {
        crate::media::card_message(nonce, size, &caption)
    };
    let delivery = kettle_ctl::show::InlineDelivery {
        permit: None,
        message: message.ok_or(InlineRefusal::TooSmall)?,
    };
    // Only a harness turns a long message into a stub; the command line
    // prints whatever the renderer's largest card takes.
    if !cli && !delivery.fits() {
        return Err(InlineRefusal::TooSmall);
    }
    pane_cards
        .insert(
            nonce,
            kettle_render::CardSpec {
                rows: size.0,
                columns: size.1,
                harness: inline.harness,
                caption,
                poster: Some(kettle_render::CardPoster::new(image)),
                duration_ms: video.map(|video| video.duration_ms),
            },
        )
        .map_err(|refusal| match refusal {
            kettle_render::CardRefusal::BadSize => InlineRefusal::TooSmall,
            kettle_render::CardRefusal::Full | kettle_render::CardRefusal::Duplicate => {
                InlineRefusal::Limit
            }
        })?;
    cards.record(
        nonce,
        crate::media::CardRecord {
            pane: pane_id,
            item,
            owner: inline.owner,
            launched: !cli,
        },
        now,
    );
    Ok(delivery)
}

/// The window holding `pane`: `ws` or one of `windows`.
fn window_with_pane<'a>(
    ws: &'a mut WindowState,
    windows: &'a mut std::collections::BTreeMap<u64, WindowState>,
    pane: u64,
) -> Option<&'a mut WindowState> {
    if ws.mux.panes.contains_key(&pane) {
        return Some(ws);
    }
    windows
        .values_mut()
        .find(|window| window.mux.panes.contains_key(&pane))
}

/// `get_state`'s `media`: whether the worker is there, and which video
/// decoders a worker here has, which is release policy, not a search: the
/// user's ffmpeg is looked for when a worker starts, and no GStreamer is used.
fn ctl_media_state(media: Option<&kettle_media::client::WorkerClient>) -> serde_json::Value {
    use kettle_media::client::MediaAvailability;
    let mut state = match media.map(kettle_media::client::WorkerClient::availability) {
        None => serde_json::json!({ "availability": "unavailable", "reason": "not_configured" }),
        Some(MediaAvailability::Checking) => serde_json::json!({ "availability": "checking" }),
        Some(MediaAvailability::Available) => serde_json::json!({ "availability": "available" }),
        Some(MediaAvailability::Unavailable(cause)) => {
            serde_json::json!({ "availability": "unavailable", "reason": cause.code() })
        }
    };
    state["video"] = serde_json::json!({
        "decoders": kettle_media::video::DECODERS,
        "gstreamer": "not_used",
    });
    state
}

fn ctl_page_values_with_page(
    req: &kettle_ctl::protocol::Request,
    key: &str,
    values: Vec<serde_json::Value>,
    page: kettle_ctl::protocol::PageRequest,
) -> kettle_ctl::protocol::Response {
    use kettle_ctl::protocol::{Response, error_codes as ec};

    if values.len() > MAX_CTL_COLLECTION_ITEMS {
        return Response::err(
            req.id,
            ec::RESPONSE_TOO_LARGE,
            "live-state collection exceeds the control enumeration budget",
        );
    }
    let snapshot = ctl_snapshot(key, &values);
    if page
        .snapshot
        .as_deref()
        .is_some_and(|want| want != snapshot)
    {
        return Response::err(
            req.id,
            ec::STALE_SNAPSHOT,
            "live state changed between pages; restart without cursor/snapshot",
        );
    }
    let total = values.len();
    let (start, end, next_cursor, truncated) = page.bounds(total);
    let mut result = serde_json::Map::new();
    result.insert(
        key.into(),
        serde_json::Value::Array(values[start..end].to_vec()),
    );
    result.insert("snapshot".into(), serde_json::Value::String(snapshot));
    result.insert("next_cursor".into(), next_cursor.into());
    result.insert("truncated".into(), truncated.into());
    result.insert("total".into(), total.into());
    Response::ok(req.id, serde_json::Value::Object(result))
}

/// A paste made while a clipboard image was still being encoded. It waits
/// its turn so pastes land in the order they were made.
struct QueuedPaste {
    window: u64,
    target: Option<u64>,
    receipt_pane: Option<u64>,
    broadcast: crate::mux::BroadcastScope,
    /// The panes a broadcast paste reaches, every window's, when it was made.
    recipients: Vec<u64>,
    content: QueuedPasteContent,
}

enum QueuedPasteContent {
    /// The PNG of encoder job `id`, once it is kept or has failed, and the
    /// clipboard's text to paste instead if it failed.
    Image {
        id: u64,
        fallback: String,
        path: Option<std::io::Result<std::path::PathBuf>>,
    },
    Text(String),
    Input {
        targets: Vec<(u64, Vec<u8>)>,
        prepare_input: bool,
    },
    Paths {
        paths: Vec<std::path::PathBuf>,
        trailing_space: bool,
        video: Option<crate::video_preview::VideoPasteRequest>,
    },
}

impl QueuedPasteContent {
    /// The bytes it holds while it waits.
    fn bytes(&self) -> usize {
        match self {
            Self::Image { fallback, .. } => fallback.len(),
            Self::Text(text) => text.len(),
            Self::Input { targets, .. } => targets
                .iter()
                .map(|(_, bytes)| bytes.len())
                .fold(0, usize::saturating_add),
            Self::Paths { paths, .. } => paths
                .iter()
                .map(|path| path.as_os_str().len())
                .fold(0, usize::saturating_add),
        }
    }

    /// Whether it is an image still being encoded.
    fn encoding(&self) -> bool {
        matches!(self, Self::Image { path: None, .. })
    }
}

/// Most pastes that may wait behind encoding images, and their text bytes.
const QUEUED_PASTES_MAX: usize = 64;
const QUEUED_PASTE_BYTES_MAX: usize = 4 * LOCAL_PASTE_MAX;

#[derive(Debug, Clone)]
pub enum UserEvent {
    Wakeup,
    /// The paste encoder finished a clipboard image's PNG.
    PastedImageEncoded,
    #[cfg(target_os = "linux")]
    Wayland,
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

/// Decode kettle's embedded 256px PNG (the dark or light variant) into the
/// *running* window's icon: the title-bar system-menu glyph (top-left), the
/// taskbar button, and the Alt-Tab thumbnail. winit leaves this icon unset by
/// default. `build.rs` embeds the same art as an `.exe` resource, but that
/// covers only Explorer, the file glyph and pinned shortcuts, not the live
/// window's `WM_SETICON`. Without this, Windows shows a generic placeholder.
/// The OS downscales the source for the small title-bar icon and picks the
/// right size for the taskbar and switcher. Best-effort: a decode failure
/// leaves the icon unset rather than aborting startup. No-op on Wayland (uses
/// the `.desktop` app_id); effective on Windows and X11.
///
/// macOS returns `None` without decoding. An AppKit window has no icon of its
/// own (the Dock and the app switcher use the bundle's), and winit's macOS
/// backend drops whatever it is given, at creation and in `set_window_icon`,
/// so the decode would only delay the first window.
fn load_window_icon(dark: bool) -> Option<winit::window::Icon> {
    #[cfg(target_os = "macos")]
    {
        let _ = dark;
        None
    }
    #[cfg(not(target_os = "macos"))]
    {
        const DARK_ICON_PNG: &[u8] = include_bytes!("../../../packaging/linux/kettle-256.png");
        const LIGHT_ICON_PNG: &[u8] =
            include_bytes!("../../../packaging/linux/kettle-light-256.png");
        let icon_png = if dark { DARK_ICON_PNG } else { LIGHT_ICON_PNG };
        let img = image::load_from_memory(icon_png).ok()?.into_rgba8();
        let (w, h) = img.dimensions();
        winit::window::Icon::from_rgba(img.into_raw(), w, h).ok()
    }
}

/// macOS `sticky = true` — make the window appear on every Space
/// (Mission Control workspace). winit 0.30 dropped the native method, so we
/// reach the underlying `NSWindow` through the raw AppKit handle and set its
/// collection behavior — exactly what `set_visible_on_all_workspaces` did. The
/// objc2 / objc2-app-kit versions are pinned to winit's own (0.5 / 0.2) so the
/// ObjC class layout matches. Best-effort: a missing handle just leaves the
/// window on its current Space.
#[cfg(target_os = "macos")]
fn set_visible_on_all_spaces(window: &winit::window::Window) {
    use objc2_app_kit::{NSView, NSWindowCollectionBehavior};
    // winit re-exports raw-window-handle, so we don't need it as a direct dep.
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};

    let Ok(handle) = window.window_handle() else {
        return;
    };
    let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
        return;
    };
    // SAFETY: on macOS winit hands us a live `NSView*`, and `resumed` runs on
    // the main thread where AppKit requires UI mutations. We only borrow it.
    let view: &NSView = unsafe { &*appkit.ns_view.as_ptr().cast::<NSView>() };
    if let Some(ns_window) = view.window() {
        // SAFETY: `setCollectionBehavior:` is an objc2 `unsafe fn` (any ObjC
        // message send is). The behavior bits are valid and the window is live.
        unsafe {
            ns_window.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::Stationary,
            );
        }
    }
}

/// Turn off AppKit's persistent UI ("Resume") before the application
/// launches. Kettle restores its own session, and AppKit cannot rebuild a
/// winit window anyway. Left on, a bundled Kettle kept encoding its window
/// state for the persistent-UI service while idle: 0.04 % CPU and 0.2 extra
/// wakeups a second. `ApplePersistence` goes in the registration domain,
/// which lives only in memory and loses to anything the user sets.
#[cfg(target_os = "macos")]
fn disable_app_state_restoration() {
    use objc2::rc::Retained;
    use objc2_foundation::{NSDictionary, NSNumber, NSString, NSUserDefaults};

    let key = NSString::from_str("ApplePersistence");
    let off = Retained::into_super(Retained::into_super(Retained::into_super(
        NSNumber::numberWithBool(false),
    )));
    let defaults = NSDictionary::from_vec(&[&*key], vec![off]);
    // SAFETY: `registerDefaults:` copies a property-list dictionary of
    // NSString keys, and this runs on the main thread before AppKit starts.
    unsafe { NSUserDefaults::standardUserDefaults().registerDefaults(&defaults) };
}

fn rect_contains(rect: kettle_render::Rect4, x: f32, y: f32) -> bool {
    x >= rect.0 && x < rect.0 + rect.2 && y >= rect.1 && y < rect.1 + rect.3
}

fn rounded_window_corner_policy(
    is_macos: bool,
    live_decorated: bool,
    live_fullscreen: bool,
) -> bool {
    is_macos && live_decorated && !live_fullscreen
}

fn rect_json(rect: kettle_render::Rect4) -> serde_json::Value {
    serde_json::json!({
        "x": rect.0,
        "y": rect.1,
        "width": rect.2,
        "height": rect.3,
    })
}

fn ctl_mouse_button(params: &serde_json::Value) -> std::result::Result<u8, String> {
    let button = params
        .get("button")
        .and_then(|v| v.as_str())
        .unwrap_or("left")
        .to_ascii_lowercase();
    match button.as_str() {
        "left" => Ok(0),
        "middle" => Ok(1),
        "right" => Ok(2),
        // The report codes the physical buttons send, not xterm's button
        // numbers: 8 and 9 would encode as a modified left or middle press.
        "back" => Ok(MOUSE_BACK),
        "forward" => Ok(MOUSE_FORWARD),
        other => Err(format!(
            "unknown button '{other}' (expected left, middle, right, back, or forward)"
        )),
    }
}

/// Terminator parity (`terminal.py:real_copy_clipboard` +
/// `config.py:smart_copy`): what `Action::Copy` writes to the clipboard.
///
///   * `selection`: the selected text, or `None` when the pane has none.
///   * `smart_copy`: `cfg.smart_copy`. With no selection, `true` (default)
///     keeps the existing clipboard and `false` clobbers it with an empty
///     string (Terminator's deliberate-UX-choice mode).
///
/// Returns the text to write, or `None` to leave the clipboard untouched.
/// On the `Some("")` clobber path the caller writes the empty string but does
/// not count it as a real copy, so the `clear_select_on_copy` follow-up is
/// skipped (no selection existed to clear). Pure; unit-testable without a
/// clipboard fixture.
fn copy_clipboard_decision(
    selection: Option<String>,
    smart_copy: bool,
) -> Result<Option<String>, std::collections::TryReserveError> {
    match (selection, smart_copy) {
        (Some(s), _) => kettle_core::scrub_card_markers(s).map(Some),
        (None, false) => Ok(Some(String::new())),
        (None, true) => Ok(None),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PasteSource {
    Clipboard,
    Primary,
}

/// Terminator parity for `putty_paste_style_source_clipboard`: right-click
/// PuTTY paste defaults to the PRIMARY selection on Linux, matching terminal
/// mouse-paste convention. Users who expect Windows/PuTTY-style clipboard
/// paste can opt into the regular clipboard source.
fn putty_paste_source(source_clipboard: bool) -> PasteSource {
    if source_clipboard {
        PasteSource::Clipboard
    } else {
        PasteSource::Primary
    }
}

/// Terminator parity (`key_zoom_in` / `key_zoom_out` via
/// Ctrl+wheel): pure decision for whether the wheel notch should resize
/// the font.
///
///   * `ctrl` — Ctrl modifier held (the canonical zoom modifier across
///     gnome-terminal, Terminator, xterm-via-ctrl-shift-plus, etc.).
///   * `notches` — whole detents drained from `WheelAccum`; sign drives
///     the zoom direction, zero short-circuits. Deliberately detents and
///     not scaled lines: one physical notch must be one font step
///     regardless of `scroll-multiplier`, and a sub-detent touchpad event
///     must not zoom at all.
///   * `disabled` — `cfg.disable_mousewheel_zoom`. Opt-out for users
///     who scroll-zoom by accident (laptop touchpads + a Ctrl-meta
///     remap is a common collision).
///
/// Returns `Some(+1)` to grow, `Some(-1)` to shrink, `None` for no-op.
/// Extracted as a pure helper so the policy is unit-testable without
/// constructing a full App + winit event loop.
fn should_zoom_font(ctrl: bool, notches: i32, disabled: bool) -> Option<i32> {
    if !ctrl || disabled || notches == 0 {
        None
    } else if notches > 0 {
        Some(1)
    } else {
        Some(-1)
    }
}

/// Terminator parity (`plugins/auto_theme.py`):
/// pick the theme to switch to on `Action::ToggleLightDark`.
///
/// Rules (case-insensitive):
///   - both `light` and `dark` set:
///       - current matches `dark`  → return `light`
///       - current matches `light` → return `dark`
///       - otherwise               → return `dark` (default landing)
///   - only one set: return that one (so a half-configured user
///     still gets a one-way switch).
///   - neither set: return `None`; dispatch logs a warn.
///
/// Pure — unit-testable without constructing a Config or App.
/// Bridge from kettle-config's `SearchCaseSensitivity`
/// to kettle-core's `CaseSensitivity`. Kept as a pure helper so
/// the two crates don't grow a circular trait dependency.
fn map_case_sensitivity(m: kettle_config::SearchCaseSensitivity) -> kettle_core::CaseSensitivity {
    match m {
        kettle_config::SearchCaseSensitivity::Smart => kettle_core::CaseSensitivity::Smart,
        kettle_config::SearchCaseSensitivity::Always => kettle_core::CaseSensitivity::Always,
        kettle_config::SearchCaseSensitivity::Never => kettle_core::CaseSensitivity::Never,
    }
}

fn search_compile_error_status(
    error: kettle_core::SearchCompileError,
) -> kettle_render::SearchStatus {
    match error {
        kettle_core::SearchCompileError::QueryTooLong { .. } => {
            kettle_render::SearchStatus::TooLong
        }
        kettle_core::SearchCompileError::InvalidRegex => kettle_render::SearchStatus::Invalid,
        kettle_core::SearchCompileError::PatternTooComplex => {
            kettle_render::SearchStatus::TooComplex
        }
    }
}

fn map_search_case_mode(
    mode: kettle_config::SearchCaseSensitivity,
) -> kettle_render::SearchCaseMode {
    match mode {
        kettle_config::SearchCaseSensitivity::Smart => kettle_render::SearchCaseMode::Smart,
        kettle_config::SearchCaseSensitivity::Always => kettle_render::SearchCaseMode::Match,
        kettle_config::SearchCaseSensitivity::Never => kettle_render::SearchCaseMode::Ignore,
    }
}

fn effective_search_status(
    search: &crate::search_input::SearchState,
) -> kettle_render::SearchStatus {
    if search.visible_truncated
        && matches!(
            search.status,
            kettle_render::SearchStatus::Match | kettle_render::SearchStatus::Wrapped
        )
    {
        kettle_render::SearchStatus::Limited
    } else {
        search.status
    }
}

/// The status the bar paints. Automation reads `effective_search_status`,
/// which reports `Searching` at once; the bar waits until a scan is actually
/// slow (see `SearchState::searching_label_at`).
fn displayed_search_status(
    search: &crate::search_input::SearchState,
    now: std::time::Instant,
) -> kettle_render::SearchStatus {
    search.painted_status(effective_search_status(search), now)
}

fn should_start_nearby_search(search: &crate::search_input::SearchState) -> bool {
    search.typing_scanned_revision != Some(search.revision)
        && !search.quiet_retry_pending
        && search.background.is_none()
}

/// Synchronize a live Search surface after a global config reload.
///
/// Returns true when the scan direction changed and the caller must derive a fresh viewport
/// anchor after resize. Closed windows consume these values the next time Search opens.
fn sync_reloaded_search_preferences(
    search: &mut crate::search_input::SearchState,
    wrap: bool,
    case_mode: kettle_config::SearchCaseSensitivity,
    invert: bool,
    now: std::time::Instant,
) -> bool {
    if !search.open {
        return false;
    }
    let wrap_changed = search.wrap != wrap;
    let case_changed = search.case_mode != case_mode;
    let direction_changed = search.invert != invert;
    search.wrap = wrap;
    search.case_mode = case_mode;
    search.invert = invert;

    if case_changed {
        search.note_edit(now);
    } else if wrap_changed || direction_changed {
        search.restart_navigation(now);
    }
    direction_changed
}

fn search_viewport_anchor(
    display_offset: usize,
    screen_lines: usize,
    last_column: usize,
    direction: kettle_core::SearchDirection,
) -> kettle_core::SearchPoint {
    let offset = i32::try_from(display_offset).unwrap_or(i32::MAX);
    match direction {
        kettle_core::SearchDirection::Forward => kettle_core::SearchPoint::new(-offset, 0),
        kettle_core::SearchDirection::Reverse => kettle_core::SearchPoint::new(
            i32::try_from(screen_lines.saturating_sub(1))
                .unwrap_or(i32::MAX)
                .saturating_sub(offset),
            last_column,
        ),
    }
}

/// Wrapping changes only where the next step goes, so the match on screen
/// stays, and a step status that described the old setting (`End reached`,
/// `Wrapped`) is cleared. With no match, search again, so turning wrap on
/// after a typing `No match` finds one past the end.
fn search_wrap_toggled(search: &mut crate::search_input::SearchState, now: std::time::Instant) {
    // A step that has already wrapped past the end would land on a match the
    // user just asked not to reach. Stop it at the edge instead, with the
    // status the step would have ended on without wrap.
    if !search.wrap
        && let Some(job) = search
            .background
            .take_if(|job| job.navigation && job.wrapped)
    {
        let unwrapped = crate::search_input::BackgroundSearch {
            wrapped: false,
            ..job
        };
        let (status, retry_at) = finished_background_search(&unwrapped, now);
        search.status = status;
        search.unlimited_retry_at = retry_at;
        return;
    }
    if search.focused.is_none() {
        search.restart_navigation(now);
    } else if matches!(
        search.status,
        kettle_render::SearchStatus::Start
            | kettle_render::SearchStatus::End
            | kettle_render::SearchStatus::Wrapped
    ) {
        search.status = kettle_render::SearchStatus::Match;
    }
}

/// Where a search restarted by a toggle begins. The match on screen, from its
/// start (its end when `invert`), so a search that still finds it keeps it.
/// A match scrolled out of `viewport` (display offset, screen lines, last
/// column) is not where the user is looking, so the search starts from the
/// viewport's edge in the current direction instead.
fn search_anchor_for(
    focused: Option<kettle_core::SearchSpan>,
    invert: bool,
    viewport: Option<(usize, usize, usize)>,
) -> Option<kettle_core::SearchPoint> {
    let on_screen = |span: &kettle_core::SearchSpan| {
        viewport.is_none_or(|(display_offset, screen_lines, _)| {
            let top = -i32::try_from(display_offset).unwrap_or(i32::MAX);
            let bottom = top.saturating_add(i32::try_from(screen_lines).unwrap_or(i32::MAX)) - 1;
            span.end.line >= top && span.start.line <= bottom
        })
    };
    if let Some(span) = focused.filter(on_screen) {
        return Some(if invert { span.end } else { span.start });
    }
    let direction = if invert {
        kettle_core::SearchDirection::Reverse
    } else {
        kettle_core::SearchDirection::Forward
    };
    viewport.map(|(display_offset, screen_lines, last_column)| {
        search_viewport_anchor(display_offset, screen_lines, last_column, direction)
    })
}

fn search_point_after(
    point: kettle_core::SearchPoint,
    direction: kettle_core::SearchDirection,
    top_line: i32,
    bottom_line: i32,
    last_column: usize,
) -> Option<kettle_core::SearchPoint> {
    match direction {
        kettle_core::SearchDirection::Forward if point.column < last_column => {
            Some(kettle_core::SearchPoint::new(point.line, point.column + 1))
        }
        kettle_core::SearchDirection::Forward if point.line < bottom_line => {
            Some(kettle_core::SearchPoint::new(point.line + 1, 0))
        }
        kettle_core::SearchDirection::Reverse if point.column > 0 => {
            Some(kettle_core::SearchPoint::new(point.line, point.column - 1))
        }
        kettle_core::SearchDirection::Reverse if point.line > top_line => {
            Some(kettle_core::SearchPoint::new(point.line - 1, last_column))
        }
        _ => None,
    }
}

fn clamp_search_point(
    point: kettle_core::SearchPoint,
    top_line: i32,
    bottom_line: i32,
    last_column: usize,
) -> kettle_core::SearchPoint {
    kettle_core::SearchPoint::new(
        point.line.clamp(top_line, bottom_line),
        point.column.min(last_column),
    )
}

fn continue_background_after_output(
    job: &mut crate::search_input::BackgroundSearch,
    token: kettle_core::SearchScanToken,
    top_line: i32,
    bottom_line: i32,
    last_column: usize,
) {
    job.token = token;
    job.output_drifted = true;
    job.cursor = clamp_search_point(job.cursor, top_line, bottom_line, last_column);
    job.wrap_anchor = clamp_search_point(job.wrap_anchor, top_line, bottom_line, last_column);
    job.edge = if job.wrapped {
        clamp_search_point(job.edge, top_line, bottom_line, last_column)
    } else {
        match job.direction {
            kettle_core::SearchDirection::Forward => {
                kettle_core::SearchPoint::new(bottom_line, last_column)
            }
            kettle_core::SearchDirection::Reverse => kettle_core::SearchPoint::new(top_line, 0),
        }
    };
}

fn bounded_search_edge(
    origin: kettle_core::SearchPoint,
    direction: kettle_core::SearchDirection,
    top_line: i32,
    bottom_line: i32,
    last_column: usize,
    max_lines: usize,
) -> kettle_core::SearchPoint {
    let lines = i32::try_from(max_lines).unwrap_or(i32::MAX);
    match direction {
        kettle_core::SearchDirection::Forward => kettle_core::SearchPoint::new(
            origin.line.saturating_add(lines).min(bottom_line),
            last_column,
        ),
        kettle_core::SearchDirection::Reverse => {
            kettle_core::SearchPoint::new(origin.line.saturating_sub(lines).max(top_line), 0)
        }
    }
}

fn wrapped_background_search(
    token: kettle_core::SearchScanToken,
    direction: kettle_core::SearchDirection,
    anchor: kettle_core::SearchPoint,
    grid_bounds: (i32, i32, usize),
    navigation: bool,
    had_focus: bool,
    output_drifted: bool,
) -> Option<crate::search_input::BackgroundSearch> {
    let (top_line, bottom_line, last_column) = grid_bounds;
    let (cursor, edge) = match direction {
        kettle_core::SearchDirection::Forward => (
            kettle_core::SearchPoint::new(top_line, 0),
            search_point_after(
                anchor,
                kettle_core::SearchDirection::Reverse,
                top_line,
                bottom_line,
                last_column,
            )?,
        ),
        kettle_core::SearchDirection::Reverse => (
            kettle_core::SearchPoint::new(bottom_line, last_column),
            search_point_after(
                anchor,
                kettle_core::SearchDirection::Forward,
                top_line,
                bottom_line,
                last_column,
            )?,
        ),
    };
    let ordered = match direction {
        kettle_core::SearchDirection::Forward => cursor <= edge,
        kettle_core::SearchDirection::Reverse => cursor >= edge,
    };
    ordered.then_some(crate::search_input::BackgroundSearch {
        token,
        direction,
        cursor,
        edge,
        wrap_anchor: anchor,
        wrapped: true,
        nearby: false,
        navigation,
        had_focus,
        output_drifted,
    })
}

fn exhausted_search_status(
    job: &crate::search_input::BackgroundSearch,
) -> kettle_render::SearchStatus {
    if job.navigation && job.had_focus && !job.wrapped {
        match job.direction {
            kettle_core::SearchDirection::Forward => kettle_render::SearchStatus::End,
            kettle_core::SearchDirection::Reverse => kettle_render::SearchStatus::Start,
        }
    } else {
        kettle_render::SearchStatus::NoMatch
    }
}

fn should_retry_output_drift(job: &crate::search_input::BackgroundSearch) -> bool {
    job.output_drifted && !job.navigation
}

fn prepare_initial_search_retry_after_output(
    search: &mut crate::search_input::SearchState,
    job: &crate::search_input::BackgroundSearch,
    anchor: kettle_core::SearchPoint,
) -> bool {
    if job.nearby {
        search.typing_scanned_revision = Some(search.revision);
        if !should_retry_output_drift(job) {
            search.unlimited_retry_at = None;
        }
    }
    if !should_retry_output_drift(job) {
        return false;
    }
    search.anchor = Some(anchor);
    search.quiet_retry_pending = true;
    true
}

fn activate_quiet_search_retry(search: &mut crate::search_input::SearchState) -> bool {
    if !search.quiet_retry_pending {
        return false;
    }
    search.quiet_retry_pending = false;
    search.typing_scanned_revision = None;
    // The nearby scan runs first. If it misses, the full scan follows on the
    // next turn instead of leaving the search at `Searching` with nothing
    // scheduled.
    search.unlimited_retry_at = Some(std::time::Instant::now());
    search.set_status_at(
        kettle_render::SearchStatus::Searching,
        std::time::Instant::now(),
    );
    true
}

fn cancel_deferred_search_retry(search: &mut crate::search_input::SearchState) {
    search.quiet_retry_pending = false;
    search.unlimited_retry_at = None;
}

fn finished_background_search(
    job: &crate::search_input::BackgroundSearch,
    now: std::time::Instant,
) -> (kettle_render::SearchStatus, Option<std::time::Instant>) {
    if job.output_drifted && !should_retry_output_drift(job) {
        // Output destroyed the stable ordering required by an explicit Previous/Next request.
        // Re-running the default-direction typing search would verify a different operation, so
        // keep the honest bounded result and let the user's next navigation start a fresh job.
        (kettle_render::SearchStatus::Limited, None)
    } else if should_retry_output_drift(job) {
        // The retained cursor guarantees progress under a continuously-writing PTY, but rows may
        // have shifted between chunks. Never turn that best-effort pass into a definitive miss;
        // once output is quiet, restart the initial search from a fresh viewport anchor.
        (
            kettle_render::SearchStatus::Searching,
            Some(now + std::time::Duration::from_millis(500)),
        )
    } else {
        (exhausted_search_status(job), None)
    }
}

/// Terminator parity (`plugins/run_cmd_on_match.py`):
/// fire the configured argv as a fire-and-forget subprocess.
///
/// Security posture:
///   - argv form (no shell). The configured command is treated as
///     data, not as a shell string — kettle never invokes `sh -c`
///     on it. A configured `trigger = .* :: rm -rf $HOME` would
///     spawn `rm -rf $HOME` *literally*, with `$HOME` as a literal
///     argv element, not as the user's home dir.
///   - empty argv ⇒ no-op (parser already rejects, but be safe).
///   - spawn errors logged but otherwise ignored so a missing
///     binary doesn't loop-fail.
///
/// Stripped into a free fn so the dispatch arm stays small.
fn spawn_trigger_command(argv: &[String]) {
    if argv.is_empty() {
        return;
    }
    static ACTIVE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let Some(slot) = TriggerCommandSlot::acquire(&ACTIVE) else {
        log::warn!("trigger run-command skipped: too many running commands");
        return;
    };
    let mut cmd = std::process::Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    // Don't capture stdout/stderr; the child inherits kettle's
    // file descriptors (where applicable). On Unix this means
    // stdout goes to wherever kettle was launched from — which
    // is acceptable for a user-configured fire-and-forget hook.
    if let Err(error) = std::thread::Builder::new()
        .name("kettle-trigger".into())
        .spawn(move || {
            let _slot = slot;
            if let Err(error) = cmd.spawn().and_then(|mut child| child.wait()) {
                log::warn!("trigger run-command spawn/wait failed: {error}");
            }
        })
    {
        log::warn!("trigger run-command reaper failed to start: {error}");
    }
}

struct TriggerCommandSlot<'a>(&'a std::sync::atomic::AtomicUsize);

impl<'a> TriggerCommandSlot<'a> {
    fn acquire(active: &'a std::sync::atomic::AtomicUsize) -> Option<Self> {
        use std::sync::atomic::Ordering::Relaxed;

        // Rust 1.99 renames fetch_update to try_update; the MSRV (1.95) predates it.
        #[allow(deprecated)]
        let acquired =
            active.fetch_update(Relaxed, Relaxed, |count| (count < 32).then_some(count + 1));
        acquired.ok().map(|_| Self(active))
    }
}

impl Drop for TriggerCommandSlot<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Named keys handled by the confirmation-dialog state machine. Lifted out
/// of winit's `NamedKey` so the pure helper isn't coupled to a UI
/// framework type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKey {
    Escape,
    Enter,
    Tab,
    ShiftTab,
    Left,
    Right,
    /// `y` (with `vim-menu-nav`): answer the dialog's QUESTION with yes,
    /// regardless of which button is focused.
    Yes,
    /// `n` (with `vim-menu-nav`): dismiss without dispatching.
    No,
}

/// Outcome of a key press in the confirm dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKeyResult {
    /// Update `focus_idx` to the new value + redraw.
    Move(usize),
    /// Dispatch `on_confirm` + close the modal.
    Confirm,
    /// Close the modal without dispatching.
    Cancel,
    /// Key wasn't a nav key for the modal — caller can pass through
    /// or ignore (we suppress non-nav input while a modal is open).
    Ignore,
}

/// The group name Terminator's `group_all` uses (`window.py:936`, `_('All')`).
///
/// A named constant so the importer, the dispatch, and any future "is
/// everything grouped?" check cannot disagree about the spelling — the name is
/// user-visible in the titlebar pill.
fn kettle_ui_group_all_name() -> &'static str {
    "All"
}

/// How much a close of the tab at `idx` would take: `(panes, busy)`.
///
/// `busy` counts only panes that are NOT sitting idle at an integrated-shell
/// prompt — an idle prompt has no work to lose. A pane with no shell
/// integration cannot prove it is idle, so it reports busy and the prompt
/// still fires; the same goes for a missing tab, where prompting is the
/// conservative answer.
fn tab_close_scope(ws: &WindowState, idx: usize) -> (usize, usize) {
    let Some(tab) = ws.mux.tabs.get(idx) else {
        return (1, 1);
    };
    let ids = tab.root.leaf_ids();
    let busy = ids
        .iter()
        .filter(|id| {
            ws.mux
                .panes
                .get(id)
                .map(|p| !p.term.shell_idle())
                .unwrap_or(true)
        })
        .count();
    (ids.len(), busy)
}

/// How much closing this whole window would take: `(panes, busy)` across
/// every tab. Same idle rule as [`tab_close_scope`].
fn window_close_scope(ws: &WindowState) -> (usize, usize) {
    (
        ws.mux.panes.len(),
        ws.mux
            .panes
            .values()
            .filter(|p| !p.term.shell_idle())
            .count(),
    )
}

/// Pure helper that maps a (current_focus, num_buttons,
/// key) tuple to the next action for the confirm-dialog state
/// machine, wired to the App's winit key handler.
fn confirm_dialog_keypress(
    current_focus: usize,
    num_buttons: usize,
    key: ConfirmKey,
) -> ConfirmKeyResult {
    if num_buttons == 0 {
        return ConfirmKeyResult::Cancel;
    }
    match key {
        ConfirmKey::Escape => ConfirmKeyResult::Cancel,
        // Enter activates the FOCUSED button, not always Confirm. The
        // close-confirm dialogs open focused on `Cancel` (index 0, the safe
        // default the renderer highlights), so firing the destructive action
        // regardless of focus would contradict that highlight and risk data
        // loss. Buttons are `[Cancel, Confirm]`, so only the last button
        // confirms; any other focused button (Cancel) cancels.
        ConfirmKey::Enter => {
            if current_focus + 1 == num_buttons {
                ConfirmKeyResult::Confirm
            } else {
                ConfirmKeyResult::Cancel
            }
        }
        ConfirmKey::Tab => ConfirmKeyResult::Move((current_focus + 1) % num_buttons),
        ConfirmKey::ShiftTab => {
            ConfirmKeyResult::Move((current_focus + num_buttons - 1) % num_buttons)
        }
        ConfirmKey::Left => {
            if current_focus == 0 {
                ConfirmKeyResult::Ignore
            } else {
                ConfirmKeyResult::Move(current_focus - 1)
            }
        }
        ConfirmKey::Right => {
            if current_focus + 1 >= num_buttons {
                ConfirmKeyResult::Ignore
            } else {
                ConfirmKeyResult::Move(current_focus + 1)
            }
        }
        // `y`/`n` (`vim-menu-nav`) answer the dialog directly. Unlike Enter,
        // which fires the FOCUSED button, `y` is an explicit answer to the
        // question, so focus is irrelevant.
        ConfirmKey::Yes => ConfirmKeyResult::Confirm,
        ConfirmKey::No => ConfirmKeyResult::Cancel,
    }
}

/// Fallback vertical-strip width used by layout helpers; production uses
/// cfg.tab_bar_width.
#[allow(dead_code)] // doc-only reference + layout-helper test fixture; production uses cfg.tab_bar_width
pub const VERTICAL_TAB_STRIP_W: f32 = 180.0;

/// The tab bar's band: where it draws and where a dragged tab docks. A bottom
/// bar sits above the search bar and a bottom status bar, and a side strip
/// stops above the search bar, so the docking target matches what is painted.
fn tab_band_rect(
    surface: (u32, u32),
    pos: TabBarPos,
    height: f32,
    strip_width: f32,
    search_height: f32,
    bottom_status_height: f32,
) -> Rect {
    let (sw, sh) = (surface.0 as f32, surface.1 as f32);
    match pos {
        TabBarPos::Top => (0.0, 0.0, sw, height),
        TabBarPos::Bottom => (
            0.0,
            sh - search_height - bottom_status_height - height,
            sw,
            height,
        ),
        TabBarPos::Left => (0.0, 0.0, strip_width, sh - search_height),
        TabBarPos::Right => (sw - strip_width, 0.0, strip_width, sh - search_height),
    }
}

/// Pure helper that computes the pane-content rect from the surface size, bar
/// metrics, and the edge each bar occupies.
///
/// Returns `(x, y, width, height)` in pixel coordinates.
///
/// A `TabBarPos::Left` or `Right` strip claims a per-side width slice
/// (`VERTICAL_TAB_STRIP_W`, 180 px) rather than a per-edge height.
///
/// Pure (no `&self`, renderer, or winit) so the layout math can be unit-tested
/// without constructing a full App. `App::area` calls
/// [`content_rect_for_with_strip`] directly with the configured strip width.
#[allow(dead_code)]
// production callers use content_rect_for_with_strip; this wrapper drives the layout-math drift guards (app.rs:9411+)
fn content_rect_for(
    surface: (u32, u32),
    tab_bar_h: f32,
    status_bar_h: f32,
    tab_bar_pos: kettle_config::TabBarPos,
    status_bar_mode: kettle_config::StatusBarMode,
) -> Rect {
    content_rect_for_with_strip(
        surface,
        ContentBands {
            tab_bar_h,
            status_bar_h,
            ..ContentBands::default()
        },
        tab_bar_pos,
        status_bar_mode,
        VERTICAL_TAB_STRIP_W,
    )
}

/// Horizontal chrome bands that reduce the pane-content area.
#[derive(Clone, Copy, Default)]
struct ContentBands {
    tab_bar_h: f32,
    status_bar_h: f32,
    vertical_title_edit_h: f32,
}

impl ContentBands {
    fn for_restore(mut self, mode: TabBarMode, tabs: usize, cell_height: f32) -> Self {
        if matches!(mode, TabBarMode::Auto) {
            self.tab_bar_h = if tabs > 1 { cell_height + 8.0 } else { 0.0 };
        }
        self
    }
}

#[cfg(test)]
mod restore_content_tests {
    use super::*;

    #[test]
    fn restored_auto_tabs_reserve_chrome_before_spawning() {
        let surface = (800, 600);
        let empty = ContentBands::default();
        let restored = empty.for_restore(TabBarMode::Auto, 2, 16.0);
        let area = content_rect_for_with_strip(
            surface,
            restored,
            TabBarPos::Top,
            kettle_config::StatusBarMode::Off,
            200.0,
        );
        assert_eq!(area, (0.0, 24.0, 800.0, 576.0));
        assert_eq!(
            grid_dims_px((area.2, area.3), (8.0, 16.0), (0.0, 0.0)),
            (100, 36)
        );
        assert_eq!(empty.for_restore(TabBarMode::Auto, 1, 16.0).tab_bar_h, 0.0);
        assert_eq!(empty.for_restore(TabBarMode::Off, 2, 16.0).tab_bar_h, 0.0);
    }
}

/// Explicit strip-width variant, so callers with `cfg.tab_bar_width` in scope
/// can pass it through. The non-`_with_strip` wrapper above keeps the same
/// signature for code paths without a Config.
fn content_rect_for_with_strip(
    surface: (u32, u32),
    bands: ContentBands,
    tab_bar_pos: kettle_config::TabBarPos,
    status_bar_mode: kettle_config::StatusBarMode,
    strip_w: f32,
) -> Rect {
    let (sw, sh) = (surface.0 as f32, surface.1 as f32);
    let tb_on_top = matches!(tab_bar_pos, kettle_config::TabBarPos::Top);
    let tb_on_bottom = matches!(tab_bar_pos, kettle_config::TabBarPos::Bottom);
    let tb_on_left = matches!(tab_bar_pos, kettle_config::TabBarPos::Left);
    let tb_on_right = matches!(tab_bar_pos, kettle_config::TabBarPos::Right);
    let sb_on_top = matches!(status_bar_mode, kettle_config::StatusBarMode::Top);
    let sb_on_bottom = matches!(status_bar_mode, kettle_config::StatusBarMode::Bottom);

    // Vertical: status bar and title editing claim y-bands (status is always
    // horizontal in v1); the persistent tab strip claims only an x-band.
    let top_offset = (if tb_on_top { bands.tab_bar_h } else { 0.0 })
        + (if sb_on_top { bands.status_bar_h } else { 0.0 })
        + (if tb_on_left || tb_on_right {
            bands.vertical_title_edit_h.max(0.0)
        } else {
            0.0
        });
    let bot_offset = (if tb_on_bottom { bands.tab_bar_h } else { 0.0 })
        + (if sb_on_bottom {
            bands.status_bar_h
        } else {
            0.0
        });
    let left_offset = if tb_on_left { strip_w } else { 0.0 };
    let right_offset = if tb_on_right { strip_w } else { 0.0 };
    let content_h = (sh - top_offset - bot_offset).max(1.0);
    let content_w = (sw - left_offset - right_offset).max(1.0);
    (left_offset, top_offset, content_w, content_h)
}

/// Build the default focused-pane screenshot path under the cache directory.
/// Uses `<cache>/kettle/shots/` (mirrors the `session_log_path` scheme below); falls
/// back to `./kettle-shots/` when no cache dir resolves.
///
/// File name shape: `kettle-<unix-secs>-<pid>-<sequence>.png`. `Action::TakeScreenshot`
/// and a ctl `screenshot` request without a `path` capture to it.
///
/// The process-wide sequence distinguishes captures sharing a timestamp.
fn session_screenshot_path(
    unix_secs: u64,
    pid: u32,
    cache_dir: Option<&std::path::Path>,
) -> std::path::PathBuf {
    static NEXT_SHOT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT_SHOT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = cache_dir
        .map(|p| p.to_path_buf().join("kettle").join("shots"))
        .unwrap_or_else(|| std::path::PathBuf::from("kettle-shots"));
    dir.join(format!("kettle-{unix_secs}-{pid}-{sequence}.png"))
}

fn poll_screenshot_completion(ws: &mut WindowState) -> Option<Result<std::path::PathBuf, String>> {
    let result = match ws.screenshot_completion.as_ref()?.try_recv() {
        Ok(result) => result,
        Err(std::sync::mpsc::TryRecvError::Empty) => return None,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
            Err("screenshot capture worker disconnected".into())
        }
    };
    ws.screenshot_completion = None;
    Some(result)
}

/// Validate a caller-supplied `screenshot` destination. `path` reaches here
/// verbatim from the ctl `screenshot` method and the `kettle_screenshot` MCP
/// tool built on it, both reachable by an agent whose instructions can be
/// steered by prompt-injected terminal content. Writing to an existing sensitive
/// file (`~/.bashrc`, `~/.ssh/authorized_keys`, a Windows Startup-folder item)
/// or through a symlink planted ahead of time would make this a silent
/// arbitrary-file-overwrite primitive.
///
/// `std::fs::symlink_metadata` (NOT `Path::exists`, which follows symlinks and
/// would miss a dangling one) detects ANY existing entry at the destination
/// (file, directory, or symlink) without following it, so a pre-planted symlink
/// is caught even when its target doesn't exist yet. Requiring a brand-new
/// destination is the same never-follow-an-existing-path discipline used
/// elsewhere in this codebase (`kettle_state::atomic_replace`, the recorder).
/// The cost is that a second capture to the same `path` fails instead of
/// clobbering the first. This probe is an up-front fast-fail; the renderer's
/// create-new publish is the security boundary.
///
/// Pure (no filesystem writes, only a metadata probe), so it is unit-testable
/// against a real temp directory.
fn validate_screenshot_path(s: &str) -> Result<std::path::PathBuf, &'static str> {
    if s.trim().is_empty() {
        return Err("'path' is empty");
    }
    let path = std::path::PathBuf::from(s);
    if std::fs::symlink_metadata(&path).is_ok() {
        return Err(
            "'path' already exists — refusing to overwrite an existing file or follow an \
             existing symlink; choose a new destination",
        );
    }
    Ok(path)
}

/// Terminator parity (`plugins/logger.py`): build the
/// per-pane session-log path. Lives under `<cache>/kettle/logs/`
/// (XDG-respecting via env probe; falls back to `./kettle-logs/`
/// when no cache dir is available).
///
/// File name shape: `kettle-<unix-secs>-<pid>-<sequence>.log`. The process-wide
/// sequence distinguishes panes and repeated logging sessions in one second.
fn session_log_path(
    unix_secs: u64,
    pid: u32,
    cache_dir: Option<&std::path::Path>,
) -> std::path::PathBuf {
    static NEXT_SESSION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT_SESSION.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let dir = cache_dir
        .map(|p| p.to_path_buf().join("kettle").join("logs"))
        .unwrap_or_else(|| std::path::PathBuf::from("kettle-logs"));
    dir.join(format!("kettle-{unix_secs}-{pid}-{sequence}.log"))
}

/// Locate the XDG cache dir for the current user without
/// pulling in the `dirs` crate. Probes `$XDG_CACHE_HOME` first
/// (the spec-canonical var) then `$HOME/.cache` on Linux/macOS,
/// then `$LOCALAPPDATA` on Windows-ish, returning `None` if none
/// are set (CI / container envs). Pure modulo the env-var reader
/// fn so tests can pin the env.
fn cache_dir_from_env<F: Fn(&str) -> Option<String>>(get: F) -> Option<std::path::PathBuf> {
    let var = |k: &str| get(k).filter(|s| !s.is_empty());
    // XDG_CACHE_HOME is the explicit cross-platform override.
    if let Some(p) = var("XDG_CACHE_HOME") {
        return Some(std::path::PathBuf::from(p));
    }
    // Per-OS fallback, matching `default_path_from`. On
    // Windows the canonical per-user cache dir is `%LOCALAPPDATA%`; a stray
    // `HOME` (git-bash / MSYS / WSL-interop export one) must NOT redirect
    // screenshots / crash logs to `~/.cache` — the same config-dir split-brain
    // that bit the config path (a shell launch vs a Start-menu launch would
    // disagree). On Unix, `HOME/.cache` is the standard XDG fallback.
    if cfg!(windows) {
        var("LOCALAPPDATA").map(std::path::PathBuf::from)
    } else {
        var("HOME").map(|h| std::path::PathBuf::from(h).join(".cache"))
    }
}

/// Horizontal tab-bar layout. Tabs divide `strip` evenly and **fill the bar**
/// (two tabs each take half). There is intentionally no max width; tabs always
/// maximize width. When `scroll` is on and they would shrink below `min_w`, the
/// bar overflows: tabs stay at `min_w`, `‹ ›` arrow buttons (each `arrow_w`
/// wide) reserve the strip ends, and only whole tabs that fit between them are
/// shown, chosen so the active tab is visible (pinned to the trailing edge once
/// it scrolls past the first page). Pure (no `&self`, renderer, or winit), so
/// it is unit-testable.
struct TabStripLayout {
    /// Per-tab width for every tab EXCEPT the last one.
    width: f32,
    /// Width of the LAST tab. Equal to `width` in every layout except the
    /// full-width-aligned one, where the last tab yields the trailing `▾`/`+`
    /// button reservation so the other tab boundaries land on the same grid as
    /// vertical pane splits.
    last_width: f32,
    /// On-screen left x per tab, or `None` when scrolled out of view.
    xs: Vec<Option<f32>>,
    /// Left x of the `‹` / `›` scroll buttons when the bar overflows.
    arrow_left: Option<f32>,
    arrow_right: Option<f32>,
}

fn tab_strip_layout(
    n: usize,
    active: usize,
    strip: f32,
    trailing_reserve: f32,
    arrow_w: f32,
    min_w: f32,
    scroll: bool,
) -> TabStripLayout {
    let n = n.max(1);
    let strip = strip.max(1.0);
    let min_w = min_w.max(1.0);
    let reserve = trailing_reserve.max(0.0);
    // Preferred layout: divide the FULL bar width (the button-trimmed `strip`
    // PLUS the trailing `▾`/`+` reservation) evenly, so tab boundaries fall on
    // the same grid as vertical pane splits — with 2 tabs the tab1/tab2
    // boundary lands at the window centre, in line with a 50/50 split's
    // divider. Only the LAST tab is shortened to yield the button reservation
    // (its right edge stops at `strip`); every other tab is `even_full` wide.
    // Used whenever the shortened last tab still clears `min_w` (so no tab is
    // driven below the floor). `reserve == 0` degrades to plain even fill.
    let full = strip + reserve;
    let even_full = full / n as f32;
    let last_full = even_full - reserve;
    if last_full >= min_w {
        let xs = (0..n).map(|i| Some(i as f32 * even_full)).collect();
        return TabStripLayout {
            width: even_full,
            last_width: last_full,
            xs,
            arrow_left: None,
            arrow_right: None,
        };
    }
    let even = strip / n as f32;
    if !scroll || even >= min_w {
        // Full-width alignment would shrink the last tab below `min_w`, so fall
        // back to the original behavior: tabs divide the button-trimmed `strip`
        // evenly and fill it, left-packed. (With scrolling off and many tabs,
        // width may fall below min_w — they just keep shrinking, no overflow.)
        let width = even;
        let xs = (0..n).map(|i| Some(i as f32 * width)).collect();
        return TabStripLayout {
            width,
            last_width: width,
            xs,
            arrow_left: None,
            arrow_right: None,
        };
    }
    // Overflow: tabs at `min_w`, arrows reserve both ends, whole tabs only. A
    // strip with less than `min_w` between its arrows still shows the active
    // tab, narrowed to that room, rather than drawn over the arrows and buttons.
    let arrow_w = arrow_w.clamp(1.0, strip / 2.0);
    let room = (strip - 2.0 * arrow_w).max(0.0);
    let width = min_w.min(room);
    let count = if width > 0.0 {
        ((room / width).floor() as usize).clamp(1, n)
    } else {
        1
    };
    let first = first_shown_tab(n, active, count);
    let x0 = arrow_w;
    let xs = (0..n)
        .map(|i| {
            if i >= first && i < first + count {
                Some(x0 + (i - first) as f32 * width)
            } else {
                None
            }
        })
        .collect();
    TabStripLayout {
        width,
        last_width: width,
        xs,
        arrow_left: Some(0.0),
        arrow_right: Some(strip - arrow_w),
    }
}

/// The first of `count` consecutive tabs, out of `n`, that an overflowing tab
/// bar shows: page one while it holds the active tab, else the window that
/// ends at the active tab. Both bar directions scroll this way, so moving the
/// selection is how a bar scrolls.
fn first_shown_tab(n: usize, active: usize, count: usize) -> usize {
    let count = count.clamp(1, n.max(1));
    if active < count {
        0
    } else {
        (active + 1 - count).min(n.max(1) - count)
    }
}

/// Where a tab bar parks the tabs it does not show: far off-screen on its
/// main axis, kept in the segment list so indices stay put.
const PARKED_TAB: f32 = -100_000.0;

/// Whether a rendered tab segment is on screen rather than parked.
fn tab_shown(seg: &kettle_render::TabSeg) -> bool {
    seg.rect.0 > PARKED_TAB / 2.0 && seg.rect.1 > PARKED_TAB / 2.0
}

/// Rows of a side tab bar with `rows` rows for its tabs and `+` (the update
/// chip's row not counted). While every tab and `+` fit, the tabs stack from
/// the top and `+` follows the last. Otherwise `⌃` and `⌄` rows bracket as
/// many whole tabs as fit, chosen by [`first_shown_tab`] so the active tab is
/// always shown, with `+` below `⌄`. A strip too short for the arrows shows
/// the active tab and `+` alone, and one with a single row (or less) the
/// active tab alone: `plus` is then `rows` or more, a row the strip lacks.
/// Pure, like [`tab_strip_layout`].
#[derive(Debug, PartialEq)]
struct SideTabRows {
    /// The first tab shown, and how many are shown from it.
    first: usize,
    shown: usize,
    /// The row of the first tab shown: 1 below `⌃`, else 0.
    top: usize,
    /// The row of `+`; `⌄` is the row above it when `arrows`.
    plus: usize,
    arrows: bool,
}

fn side_tab_rows(n: usize, active: usize, rows: usize) -> SideTabRows {
    let n = n.max(1);
    if n < rows {
        return SideTabRows {
            first: 0,
            shown: n,
            top: 0,
            plus: n,
            arrows: false,
        };
    }
    if rows < 4 {
        let shown = rows.saturating_sub(1).clamp(1, n);
        return SideTabRows {
            first: first_shown_tab(n, active, shown),
            shown,
            top: 0,
            plus: shown,
            arrows: false,
        };
    }
    let shown = rows - 3;
    SideTabRows {
        first: first_shown_tab(n, active, shown),
        shown,
        top: 1,
        plus: shown + 2,
        arrows: true,
    }
}

/// Split the trailing new-tab button into `(arrow_left, plus_right)`
/// hit-rects — the `▾` dropdown on the LEFT, the `+` on the RIGHT. `arrow_w` is
/// clamped to the button width so a degenerate (tiny) button can't yield a
/// negative `+` width. Pure → unit-tested and shared by the renderer geometry
/// + the click hit-test (single source of truth).
fn split_new_tab_button(
    button: kettle_render::Rect4,
    arrow_w: f32,
) -> (kettle_render::Rect4, kettle_render::Rect4) {
    let (x, y, w, h) = button;
    let aw = arrow_w.clamp(0.0, w);
    ((x, y, aw, h), (x + aw, y, (w - aw).max(0.0), h))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum NewTabHoverTarget {
    Menu,
    Plus,
}

fn new_tab_hover_target(bar: &TabBar, x: f32, y: f32) -> Option<NewTabHoverTarget> {
    if bar.new_tab_menu.2 > 0.0 && rect_contains(bar.new_tab_menu, x, y) {
        Some(NewTabHoverTarget::Menu)
    } else if rect_contains(bar.new_tab, x, y) {
        Some(NewTabHoverTarget::Plus)
    } else {
        None
    }
}

/// Whether a pointer event belongs to the tab-bar chrome. `bar.height` is the
/// row height, not the full extent of a vertical strip, so using
/// `bar.y..bar.y + bar.height` silently makes every vertical row after the
/// first (including the new-tab button) non-interactive. `band` is the layout's
/// actual horizontal row or vertical column and is shared with rendering.
fn tab_bar_pointer_region_contains(bar: &TabBar, x: f32, y: f32) -> bool {
    bar.height > 0.0 && rect_contains(bar.band, x, y)
}

/// Terminator parity (key_next_profile / key_previous_profile):
/// pick the next profile name after `current`, wrapping at the end.
/// If `current` isn't in the list (e.g. user launched without `--profile`,
/// or with an explicit `--config FILE`), the cycle starts at index 0.
/// Pure — unit-testable without touching disk.
fn pick_next_profile(current: Option<&str>, names: &[String], forward: bool) -> String {
    let n = names.len();
    let cur_idx = current
        .and_then(|c| names.iter().position(|x| x == c))
        .unwrap_or(0);
    let next_idx = if forward {
        (cur_idx + 1) % n
    } else {
        (cur_idx + n - 1) % n
    };
    names[next_idx].clone()
}

fn pick_light_dark_target(current: &str, light: &str, dark: &str) -> Option<String> {
    let cur = current.trim().to_ascii_lowercase();
    let l = light.trim();
    let d = dark.trim();
    match (l.is_empty(), d.is_empty()) {
        (true, true) => None,
        (false, true) => Some(l.to_string()),
        (true, false) => Some(d.to_string()),
        (false, false) => {
            // Current dark switches to light. Light and any third-party theme
            // switch to dark, so only the dark match needs its own arm.
            if cur == d.to_ascii_lowercase() {
                Some(l.to_string())
            } else {
                Some(d.to_string())
            }
        }
    }
}

/// The hint winit gets at window creation, which on macOS is not the hint the
/// window ends up with.
///
/// macOS gets `None` here and picks its appearance up later, from
/// [`native_theme_sync_is_due`]. winit applies a creation-time hint as an
/// `NSWindow` appearance override before AppKit has built the titlebar's
/// container view, and the native material view kettle adds to the frame view
/// right afterwards then locks in the container-less layout: AppKit centres
/// the title over the 72pt traffic-light cluster instead of the caption, so
/// `~ — kettle` is drawn across the red and yellow buttons and clipped at the
/// window edge (Reddimus/kettle#251).
///
/// Every other platform still seeds the caption at creation, where it costs
/// nothing and avoids a first-frame flash.
///
/// Pure — unit-testable without a window.
fn creation_theme_hint(hint: Option<WindowTheme>, is_macos: bool) -> Option<WindowTheme> {
    if is_macos { None } else { hint }
}

/// Whether this window may be handed the native theme hint yet.
///
/// Only the *first* appearance a macOS window is given has to wait for the
/// window to be on screen and key, which is when AppKit builds its titlebar
/// container. That one can leave the window permanently without a caption —
/// removing the material view again does not bring it back — so a window that
/// has never been focused keeps the system appearance rather than a broken
/// caption. See [`creation_theme_hint`].
///
/// Every later change is unconditional. The caption exists by then, and this
/// is the path the runtime light/dark toggle always took. Holding those back
/// too would strand a background window on a stale appearance until someone
/// clicked it, since a schedule boundary, Lua, or a config reload can move the
/// hint while another window has focus.
///
/// Pure — unit-testable without a window.
fn native_theme_sync_is_due(window_is_key: bool, already_synced: bool, is_macos: bool) -> bool {
    !is_macos || window_is_key || already_synced
}

/// Decide the native window-theme hint (winit
/// `WindowAttributes::preferred_theme` / `Window::set_theme`) so the OS
/// titlebar — the Windows DWM caption, the Wayland Adwaita CSD frame —
/// matches the palette kettle is actually rendering, instead of staying on
/// the OS-wide setting under a forced dark/light theme.
///
///   - `Auto` without a schedule → `None`: the palette follows the OS, so
///     let the titlebar follow the OS natively too. Forcing a value here
///     would also stop winit's `ThemeChanged` events on some backends,
///     which the palette auto-switcher depends on.
///   - everything else (Explicit / Light / Dark / Auto+schedule) → the
///     titlebar matches the active theme's background darkness.
///
/// Pure — unit-testable without a window.
fn native_theme_hint(
    mode: kettle_config::ThemeMode,
    has_schedule: bool,
    theme_is_dark: bool,
) -> Option<WindowTheme> {
    if mode == kettle_config::ThemeMode::Auto && !has_schedule {
        None
    } else if theme_is_dark {
        Some(WindowTheme::Dark)
    } else {
        Some(WindowTheme::Light)
    }
}

/// Cap an OSC 52 clipboard payload so a hostile program can't make the
/// terminal allocate/set an unbounded clipboard. Truncates on a UTF-8
/// char boundary at or below `max` bytes (xterm/kitty also bound this).
fn clamp_osc52(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// 1 MiB — generous for real copies, bounded against abuse.
const OSC52_MAX: usize = 1 << 20;

/// 4 MiB limit on a *local* clipboard paste. The OSC 52 cap above guards
/// against a hostile remote program pushing an unbounded payload into
/// the system clipboard; this guards the reverse direction — a user
/// accidentally pastes a multi-GB file and kettle would otherwise feed
/// every byte into the PTY in one shot, freezing the terminal until
/// the program at the other end (cat? vim?) drained the pipe. 4 MiB
/// fits any realistic code-review / log-snippet paste with room to spare.
/// Larger payloads are rejected with visible feedback rather than truncated,
/// since silently shortening a command, patch, or encoded file is unsafe.
const LOCAL_PASTE_MAX: usize = 4 << 20;

fn local_paste_within_limit(text: &str) -> bool {
    text.len() <= LOCAL_PASTE_MAX
}

fn group_scope_is_globally_stale(
    autoclean: bool,
    scope: &BroadcastScope,
    populated_groups: &HashSet<&str>,
) -> bool {
    autoclean
        && matches!(scope, BroadcastScope::Group(name) if !populated_groups.contains(name.as_str()))
}

fn any_group_scope_needs_hoover<'a>(windows: impl IntoIterator<Item = &'a WindowState>) -> bool {
    windows.into_iter().any(|window| {
        window.mux.autoclean_groups && matches!(window.mux.broadcast, BroadcastScope::Group(_))
    })
}

fn stamp_accepted_input(
    last_typed: &mut Option<std::time::Instant>,
    delivery: PaneInputDelivery,
    input_time: std::time::Instant,
) {
    if delivery.accepted {
        *last_typed = Some(input_time);
    }
}

/// The pane a single-pane paste goes to: the focused one. Broadcast names no
/// pane and keeps its live group semantics.
fn paste_target(mux: &Mux) -> Option<u64> {
    if mux.is_broadcast_on() {
        None
    } else {
        mux.active_focus()
    }
}

/// Apply a terminal control, such as Reset's RIS or Clear Scrollback's
/// CSI 3 J, to each target pane's terminal.
///
/// Nothing is written to the child, so read-only panes are included. This is
/// VTE's model, which Terminator's Reset uses: `vte_terminal_reset` changes the
/// terminal and ignores `input-enabled`.
fn apply_terminal_control(mux: &Mux, targets: &[u64], bytes: &[u8]) {
    for id in targets {
        if let Some(pane) = mux.panes.get(id)
            && !pane.term.apply_local_output(bytes)
        {
            log::debug!("pane {id}: terminal control dropped; its reader is busy or stopped");
        }
    }
}

/// Deliver a confirmed non-broadcast text paste to the pane named when the
/// prompt opened. Mode lookup and delivery use the same stable id.
fn paste_text_into_target(
    mux: &mut Mux,
    target: Option<u64>,
    text: &str,
) -> Option<PaneInputResult> {
    let pane = mux.panes.get_mut(&target?)?;
    let bracketed = pane
        .term
        .term
        .lock()
        .ok()
        .map(|term| term.mode().contains(kettle_core::TermMode::BRACKETED_PASTE))
        .unwrap_or(false);
    let bytes = input::paste_payload(text, bracketed);
    Some(pane.feed_input(&bytes))
}

/// Deliver a confirmed non-broadcast path paste to the pane named when the
/// prompt opened. Shell formatting, mode lookup, and delivery all use it.
fn paste_paths_into_target(
    mux: &mut Mux,
    target: Option<u64>,
    paths: &[std::path::PathBuf],
    trailing_space: bool,
) -> Option<PaneInputDelivery> {
    let pane = mux.panes.get_mut(&target?)?;
    let mut text = crate::mux::format_paths_for_paste(&pane.argv, paths);
    if trailing_space {
        text.push(' ');
    }
    let bracketed = pane
        .term
        .term
        .lock()
        .ok()
        .map(|term| term.mode().contains(kettle_core::TermMode::BRACKETED_PASTE))
        .unwrap_or(false);
    let bytes = input::paste_payload(&text, bracketed);
    let result = pane.feed_input(&bytes);
    Some(PaneInputDelivery {
        result,
        accepted: result.is_queued(),
        receipt_accepted: result.is_queued(),
    })
}

/// Pure: when the mouse is over chrome (tab bar, status bar, update banner, or
/// any modal overlay), the OS cursor should be the standard arrow rather than
/// the text I-beam. Matches iTerm2 / WezTerm / Ghostty / kitty: chrome surfaces
/// are clickable, not selectable, so the I-beam is visually misleading there.
/// Returns `Some(Default)` for chrome, `None` to let the content-area caller
/// decide between `Pointer` (URL-hover) and `Text`.
fn chrome_cursor_icon(in_chrome_band: bool, modal_open: bool) -> Option<CursorIcon> {
    if in_chrome_band || modal_open {
        Some(CursorIcon::Default)
    } else {
        None
    }
}

/// Pure: when the cursor is over a tab's `✕` close-button zone, override the
/// chrome `Default` with `Pointer` — the same "hand" icon every browser
/// (Chrome / Firefox / Safari) uses to telegraph "this glyph is clickable."
/// Composes with `chrome_cursor_icon`: close-hover wins over the bar's
/// `Default` so the affordance is visible the moment the cursor lands on
/// the close zone. Returns `None` when not over a close button so the
/// content-area decision still gets a chance.
fn tab_close_hover_icon(over_close: bool) -> Option<CursorIcon> {
    if over_close {
        Some(CursorIcon::Pointer)
    } else {
        None
    }
}

/// Pure: while a tab-drag gesture is armed or in
/// progress, override every other cursor decision with the "holding
/// something" hand: `Grab` the moment a press arms the FSM (the gesture
/// could still resolve to a plain click), `Grabbing` once movement promotes
/// it to a real drag. In-window reorder and cross-window tear share
/// `detach::DragState`, so one helper covers both. `None` from `Idle`
/// defers to every other rule. No `detachable-tabs` re-check needed: the
/// press handler only arms the FSM when the config gate is on, so a
/// non-`Idle` state already implies it (see
/// `detachable_tabs_config_gates_all_detach_paths`).
fn tab_drag_cursor_icon(state: &crate::detach::DragState) -> Option<CursorIcon> {
    match state {
        crate::detach::DragState::Idle => None,
        crate::detach::DragState::ArmedInside { .. } => Some(CursorIcon::Grab),
        crate::detach::DragState::DraggingInside { .. }
        | crate::detach::DragState::DraggingOutside { .. } => Some(CursorIcon::Grabbing),
    }
}

/// The text a confirmation button shows. Painting and hit testing both use it,
/// so a click lands on what is drawn in every language.
fn confirm_button_label<'a>(button: &'a ConfirmButton, tr: &kettle_i18n::Translator) -> &'a str {
    match button {
        ConfirmButton::Cancel => tr.text(kettle_i18n::Text::ConfirmButtonCancel),
        ConfirmButton::Confirm { label, .. } => label.as_str(),
    }
}

/// The title editor's label for `scope`. The overlay paints it and the
/// input-method placement measures it, so both use this.
fn title_edit_label(scope: TitleEditScope, tr: &kettle_i18n::Translator) -> &'static str {
    use kettle_i18n::Text;
    tr.text(match scope {
        TitleEditScope::Window => Text::EditTitleWindow,
        TitleEditScope::Tab => Text::EditTitleTab,
        TitleEditScope::Pane => Text::EditTitlePane,
        TitleEditScope::Group => Text::EditTitleGroup,
    })
}

/// The column of the title editor's caret: the painted prefix in the UI's
/// language, the input, then any composition before the focus point.
fn title_edit_caret_cols(
    scope: TitleEditScope,
    input: &str,
    preedit_before_focus: &str,
    tr: &kettle_i18n::Translator,
) -> usize {
    use unicode_width::UnicodeWidthStr as _;
    kettle_render::title_edit_prefix(title_edit_label(scope, tr)).width()
        + input.width()
        + preedit_before_focus.width()
}

/// Where the title-edit overlay is painted.
///
/// Pure so the geometry can be tested.
///
/// Under a VERTICAL tab bar this must span the full window width, not the tab
/// strip. The renderer composes
/// `"  ✎ {label} {input}_   (Enter apply · Esc cancel)"` into
/// `overlay_label_cols(rect.2, cw)`; a strip is `tab-bar-width` wide (default
/// 180px, ~22 columns) and the trailing hint alone is 30 columns, so the budget
/// cannot hold even the prefix. The input would render zero columns (no text,
/// no caret, no horizontal scroll) while Enter still commits the invisible
/// buffer. For `EditPaneGroup` that buffer names the broadcast group, so it
/// decides which panes receive subsequent keystrokes.
fn title_edit_rect_for(
    vertical: bool,
    bar_y: f32,
    bar_height: f32,
    content_y: f32,
    sw: f32,
    sh: f32,
    row_h: f32,
) -> kettle_render::Rect4 {
    if bar_height > 0.0 {
        if vertical {
            let height = row_h.max(0.0).min(sh);
            let y = (content_y - height).clamp(0.0, (sh - height).max(0.0));
            (0.0, y, sw, height)
        } else {
            (0.0, bar_y, sw, bar_height)
        }
    } else {
        (0.0, 0.0, sw, row_h)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfirmAccessibilityAction {
    Focus(usize),
    Activate(usize),
    Scroll(isize),
}

fn confirmation_accessibility_action(
    target: NodeId,
    action: AccessibilityAction,
    count: usize,
) -> Option<ConfirmAccessibilityAction> {
    if let Some(index) = accessibility_confirm_button_index(target) {
        if index >= count {
            return None;
        }
        return match action {
            AccessibilityAction::Focus => Some(ConfirmAccessibilityAction::Focus(index)),
            AccessibilityAction::Click => Some(ConfirmAccessibilityAction::Activate(index)),
            _ => None,
        };
    }
    if target == accessibility_modal_id(1, 0) {
        return match action {
            AccessibilityAction::ScrollUp => Some(ConfirmAccessibilityAction::Scroll(-1)),
            AccessibilityAction::ScrollDown => Some(ConfirmAccessibilityAction::Scroll(1)),
            _ => None,
        };
    }
    None
}

fn confirm_dialog_overlay(
    dialog: &ConfirmDialogState,
    tr: &kettle_i18n::Translator,
) -> kettle_render::ConfirmDialogOverlay {
    kettle_render::ConfirmDialogOverlay {
        heading: dialog.heading.clone(),
        body: dialog.body.clone(),
        buttons: dialog
            .buttons
            .iter()
            .map(|button| kettle_render::ConfirmDialogButton {
                label: confirm_button_label(button, tr).to_string(),
                destructive: matches!(
                    button,
                    ConfirmButton::Confirm {
                        destructive: true,
                        ..
                    }
                ),
            })
            .collect(),
        focus_idx: dialog.focus_idx,
        hover_idx: None,
        pressed_idx: dialog.space_pressed,
        scroll: dialog.scroll,
    }
}

fn confirm_dialog_button_hit(
    layout: &kettle_render::ConfirmDialogLayout,
    px: f32,
    py: f32,
) -> Option<usize> {
    layout
        .buttons
        .iter()
        .position(|&rect| rect_contains(rect, px, py))
}

fn confirmation_space_key(key: &Key) -> bool {
    key == &Key::Named(NamedKey::Space) || matches!(key, Key::Character(text) if text == " ")
}

fn confirmation_space_release_target(
    armed: &mut Option<usize>,
    focused: usize,
    ui_owned: bool,
) -> Option<usize> {
    armed.take().filter(|index| ui_owned && *index == focused)
}

fn confirm_release_target(
    armed: Option<(usize, (f32, f32, f32, f32))>,
    layout: &kettle_render::ConfirmDialogLayout,
    px: f32,
    py: f32,
) -> Option<usize> {
    let (index, original_rect) = armed?;
    (layout.buttons.get(index) == Some(&original_rect)
        && confirm_dialog_button_hit(layout, px, py) == Some(index))
    .then_some(index)
}

/// Pure: which tab segment's close-button (`✕`) rect contains the cursor,
/// if any. Walks `segments` once and returns the first hit (segments don't
/// overlap in the tab bar layout; cycle-tab-bar invariant). Used both to
/// drive the OS pointer-cursor swap and the renderer's hover-background
/// quad — the visual affordance that makes the `✕` read as a button rather
/// than a trailing character in the title text.
fn hovered_close_button(segments: &[kettle_render::TabSeg], px: f32, py: f32) -> Option<usize> {
    let in_rect = |(rx, ry, rw, rh): (f32, f32, f32, f32)| {
        px >= rx && px < rx + rw && py >= ry && py < ry + rh
    };
    segments.iter().find(|s| in_rect(s.close)).map(|s| s.idx)
}

/// Per-pane titlebar hit test (Terminator parity): returns the id of the pane
/// whose titlebar band contains the click, or `None`. Pure so the hit test
/// behind `App::pane_at_titlebar_click` can be drift-guarded.
#[allow(clippy::type_complexity)]
pub(crate) fn pane_titlebar_hit(
    px: f32,
    py: f32,
    pane_rects: &[(u64, (f32, f32, f32, f32))],
    title_at_bottom: bool,
    bar_h: f32,
) -> Option<u64> {
    for (id, (rx, ry, rw, rh)) in pane_rects {
        let (bar_top, bar_bot) = if title_at_bottom {
            (*ry + *rh - bar_h, *ry + *rh)
        } else {
            (*ry + 1.0, *ry + 1.0 + bar_h)
        };
        if px >= *rx && px < *rx + *rw && py >= bar_top && py < bar_bot {
            return Some(*id);
        }
    }
    None
}

/// SGR mouse base code for the extra "side" mouse buttons, or `None` for any
/// button kettle handles locally (left/middle/right) or doesn't forward.
///
/// Forwarding these lets a 5-button mouse's Back / Forward reach a
/// mouse-tracking TUI (tmux/vim bindings, pagers). xterm encodes buttons 8–11
/// as `128 + (button - 8)`; winit's `Back` is XBUTTON1 (button 8 → 128) and
/// `Forward` is XBUTTON2 (button 9 → 129). These have no local UI meaning, so
/// they only do anything while mouse tracking is on.
fn extra_mouse_sgr(button: MouseButton) -> Option<u8> {
    match button {
        MouseButton::Back => Some(MOUSE_BACK),
        MouseButton::Forward => Some(MOUSE_FORWARD),
        _ => None,
    }
}

/// The report code of xterm's button 8, the Back button.
const MOUSE_BACK: u8 = 128;
/// The report code of xterm's button 9, the Forward button.
const MOUSE_FORWARD: u8 = 129;

/// What a mouse event did on its way to a pane's terminal.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MouseReport {
    /// A report went to the terminal.
    Written,
    /// Kettle kept the event from the terminal, and nothing else handles it.
    Swallowed,
    /// No report was due; Kettle handles the event itself.
    Declined,
}

impl MouseReport {
    /// Whether nothing else should handle the event.
    fn consumed(self) -> bool {
        self != Self::Declined
    }
}

/// Where a mouse report lands in its pane.
#[derive(Clone, Copy, Debug, PartialEq)]
enum MouseAt {
    /// The pointer's cell in the pane's rect.
    Pointer(Rect),
    /// The drag or release of a held button whose press the pane's program
    /// got: the pointer's cell in its rect while it is on screen, else the
    /// cell it was last told of.
    Held {
        rect: Option<Rect>,
        cell: (usize, usize),
    },
}

/// Whether an OSC 7 working directory is safe to turn into a `file://` URL and
/// hand to the OS opener.
///
/// The cwd comes from untrusted PTY output (`parse_osc7`
/// stores whatever a program reports), and a crafted `file://x//evil.host/share`
/// yields a stored cwd of `//evil.host/share`. Building `file://{cwd}` from that
/// produces a UNC-style URL that, when opened on Windows, connects to the
/// attacker over SMB and leaks the user's NTLM hash. The
/// `is_safe_url` locality check already rejects the resulting URL, but this is
/// the one call site that *constructs* `file://` from raw untrusted input, so
/// it gets an explicit local-only guard too (defense-in-depth). Crucially this
/// check does NO filesystem stat — `Path::is_dir` on a `//host` path would
/// itself route over SMB — it's a pure string check: reject UNC/authority
/// (`//`, leading `\`) and traversal.
fn cwd_is_local(cwd: &str) -> bool {
    !cwd.is_empty() && !cwd.starts_with("//") && !cwd.starts_with('\\') && !cwd.contains("..")
}

/// What opening a local file link would do, decided on the opener thread.
#[derive(Debug, PartialEq)]
enum FileLinkCheck {
    /// Open this resolved path: a document or a folder.
    Open(std::path::PathBuf),
    /// Refuse: it is a program or a shortcut. Holds the name to report.
    Program(String),
    /// Refuse quietly: it does not resolve to a local file or folder.
    Unresolved,
}

/// Whether opening the local file `path` would run a program or follow a
/// shortcut. Symlinks are resolved first, because the OS opener follows them:
/// `notes.md` linking to an application must not launch it. Refused when
/// either the name or the resolved name names a program or a shortcut (see
/// [`kettle_core::links::names_program_or_shortcut`]), when a folder is a
/// macOS bundle by its contents, when a file is a shortcut by its first bytes
/// (a macOS alias, which the opener follows whatever its name, or a Linux
/// launcher), or when an executable file lacks a known document extension:
/// macOS runs `a.out` in Terminal. A document whose executable bits are set,
/// as every file on a FAT or exFAT volume is, still opens.
///
/// This looks at an untrusted path, which can stall on a network mount, so
/// it runs off the UI thread. What it resolves is what opens, but a local
/// process that rewrites the file between the check and the open is not
/// stopped.
fn check_file_link(path: &std::path::Path) -> FileLinkCheck {
    use kettle_core::links::names_program_or_shortcut;
    let Ok(resolved) = std::fs::canonicalize(path) else {
        return FileLinkCheck::Unresolved;
    };
    let Some(resolved) = local_resolved_path(resolved) else {
        return FileLinkCheck::Unresolved;
    };
    let Ok(metadata) = std::fs::metadata(&resolved) else {
        return FileLinkCheck::Unresolved;
    };
    let is_dir = metadata.is_dir();
    let program = names_program_or_shortcut(path, is_dir)
        || names_program_or_shortcut(&resolved, is_dir)
        || (is_dir && resolved.join("Contents").join("Info.plist").exists())
        || (metadata.is_file()
            && (starts_like_a_shortcut(&resolved)
                || (is_executable(&metadata) && !names_a_document(&resolved))));
    if program {
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        FileLinkCheck::Program(name)
    } else {
        FileLinkCheck::Open(resolved)
    }
}

/// `resolved` as a path the opener may take: on Windows a canonical path is
/// a verbatim `\\?\C:\…`, kept as `C:\…`, while a share (`\\?\UNC\…`) or a
/// volume GUID is refused. Elsewhere, any absolute path.
fn local_resolved_path(resolved: std::path::PathBuf) -> Option<std::path::PathBuf> {
    if !cfg!(windows) {
        return resolved.is_absolute().then_some(resolved);
    }
    let text = resolved.to_str()?;
    let drive = |path: &str| {
        let b = path.as_bytes();
        b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && matches!(b[2], b'\\' | b'/')
    };
    let local = text.strip_prefix(r"\\?\").unwrap_or(text);
    (drive(local) && win32_keeps_path(local)).then(|| std::path::PathBuf::from(local))
}

/// Whether Win32 reads the drive path `local` as written once the verbatim
/// `\\?\` prefix is gone: no name ends with a dot or a space, which Win32
/// drops, and none is a reserved device name (`CON`, `NUL`, `COM1`…), which
/// Win32 opens as the device.
fn win32_keeps_path(local: &str) -> bool {
    local.split(['\\', '/']).skip(1).all(|name| {
        let stem = name.split('.').next().unwrap_or(name).trim_end();
        let device = matches!(
            stem.to_ascii_uppercase().as_str(),
            "CON" | "PRN" | "AUX" | "NUL"
        ) || (stem.len() == 4
            && ["COM", "LPT"].iter().any(|prefix| {
                stem.get(..3)
                    .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
            })
            && matches!(stem.as_bytes()[3], b'1'..=b'9'));
        !name.ends_with(['.', ' ']) && !device
    })
}

/// Whether a file's mode lets it run (Unix); never on Windows, which runs by
/// extension.
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        false
    }
}

/// Whether `path` has the extension of a common document, image, media,
/// archive, text or source file, which the OS opener shows in an app rather
/// than runs even when its executable bits are set.
fn names_a_document(path: &std::path::Path) -> bool {
    const DOCUMENTS: &[&str] = &[
        "pdf", "png", "jpg", "jpeg", "gif", "webp", "svg", "bmp", "tif", "tiff", "heic", "ico",
        "psd", "mp4", "mov", "m4v", "mkv", "webm", "avi", "mp3", "m4a", "wav", "flac", "ogg",
        "opus", "aac", "txt", "md", "markdown", "rst", "adoc", "org", "tex", "html", "htm", "css",
        "json", "jsonl", "yaml", "yml", "toml", "xml", "csv", "tsv", "log", "rtf", "doc", "docx",
        "xls", "xlsx", "ppt", "pptx", "odt", "ods", "odp", "pages", "numbers", "key", "epub",
        "zip", "tar", "gz", "tgz", "bz2", "xz", "zst", "7z", "rar", "c", "h", "cc", "cpp", "hpp",
        "rs", "go", "java", "kt", "swift", "ts", "tsx", "jsx", "sql", "ini", "conf", "cfg", "lock",
        "ipynb", "diff", "patch", "mmd", "dot",
    ];
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            DOCUMENTS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// Whether a file is a shortcut an opener follows: a macOS alias, by its
/// Finder flag (a classic alias keeps its data in the resource fork) or by
/// its bookmark data, or a Linux desktop launcher, whose first group, past
/// blank and comment lines, is `[Desktop Entry]`.
fn starts_like_a_shortcut(path: &std::path::Path) -> bool {
    use std::io::Read as _;
    if finder_alias_flag(path) {
        return true;
    }
    let mut head = Vec::with_capacity(4096);
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    if file.take(4096).read_to_end(&mut head).is_err() {
        return false;
    }
    if head.starts_with(b"book") && head.get(8..12) == Some(b"mark") {
        return true;
    }
    let head = head.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(&head);
    head.split(|&byte| byte == b'\n')
        .map(<[u8]>::trim_ascii)
        .find(|line| !line.is_empty() && !line.starts_with(b"#"))
        .is_some_and(|group| {
            [
                &b"[Desktop Entry]"[..],
                b"[Desktop Action",
                b"[KDE Desktop Entry]",
            ]
            .iter()
            .any(|launcher| group.starts_with(launcher))
        })
}

/// Whether macOS marks `path` as a Finder alias (`kIsAlias` in the Finder
/// flags of its `com.apple.FinderInfo` attribute).
#[cfg(target_os = "macos")]
fn finder_alias_flag(path: &std::path::Path) -> bool {
    use std::os::unix::ffi::OsStrExt as _;
    const IS_ALIAS: u16 = 0x8000;
    let Ok(c_path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut info = [0u8; 32];
    // SAFETY: both names are NUL-terminated C strings that outlive the call,
    // and `info` is writable for the length passed.
    let read = unsafe {
        libc::getxattr(
            c_path.as_ptr(),
            c"com.apple.FinderInfo".as_ptr(),
            info.as_mut_ptr().cast(),
            info.len(),
            0,
            0,
        )
    };
    read >= 10 && u16::from_be_bytes([info[8], info[9]]) & IS_ALIAS != 0
}

#[cfg(not(target_os = "macos"))]
fn finder_alias_flag(_path: &std::path::Path) -> bool {
    false
}

/// At most this many file links are checked and opened at once; a stalled
/// network path cannot pile up threads click by click.
const MAX_FILE_LINK_OPENERS: usize = 4;
static FILE_LINK_OPENERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A slot among `limit` concurrent users of `counter`, released on drop, or
/// `None` when all are taken.
fn take_opener_slot(
    counter: &'static std::sync::atomic::AtomicUsize,
    limit: usize,
) -> Option<OpenerSlot> {
    use std::sync::atomic::Ordering;
    // Rust 1.99 renames fetch_update to try_update; the MSRV (1.95) predates it.
    #[allow(deprecated)]
    let taken = counter.fetch_update(Ordering::AcqRel, Ordering::Acquire, |taken| {
        (taken < limit).then_some(taken + 1)
    });
    taken.ok().map(|_| OpenerSlot(counter))
}

struct OpenerSlot(&'static std::sync::atomic::AtomicUsize);

impl Drop for OpenerSlot {
    fn drop(&mut self) {
        self.0.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}

/// Check the local file link `path` and open what it resolves to, on its own
/// thread: with the custom URL handler, if one is set, given a file URL for
/// the resolved path, or with the system opener. A program or shortcut is
/// refused with a notification.
fn spawn_file_link_opener(
    path: std::path::PathBuf,
    handler: Option<String>,
    tr: kettle_i18n::Translator,
) {
    let Some(slot) = take_opener_slot(&FILE_LINK_OPENERS, MAX_FILE_LINK_OPENERS) else {
        log::warn!("too many file links are still opening; ignoring this one");
        return;
    };
    let spawned = std::thread::Builder::new()
        .name("kettle-open-link".into())
        .spawn(move || {
            let _slot = slot;
            match check_file_link(&path) {
                FileLinkCheck::Program(name) => fire_notify(
                    tr.text(kettle_i18n::Text::NotifyTitleProgramNotOpened),
                    &tr.notify_body_program_not_opened(&name),
                ),
                FileLinkCheck::Unresolved => {
                    log::warn!("not opening a file link that names no local file or folder");
                    // Say so: from behind tmux this is usually a path an ssh
                    // inside it printed, which names a file on that machine.
                    let name = kettle_core::links::file_url_for_path(&path)
                        .map(|url| link_display_name(&url, 48))
                        .unwrap_or_default();
                    fire_notify(
                        tr.text(kettle_i18n::Text::NotifyTitleRemoteFileNotOpened),
                        &tr.notify_body_file_link_not_found(&name),
                    );
                }
                FileLinkCheck::Open(resolved) => open_checked_file(&resolved, handler.as_deref()),
            }
        });
    if let Err(e) = spawned {
        log::warn!("could not start the link opener: {e}");
    }
}

// Start the waiter before its child so a thread-spawn failure cannot lose a child handle.
fn spawn_custom_url_handler(cmd: &str, target: &str) -> std::io::Result<()> {
    let cmd = cmd.to_string();
    let target = target.to_string();
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("kettle-url-handler".into())
        .spawn(move || {
            match std::process::Command::new(&cmd)
                .arg(&target)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
            {
                Ok(mut child) => {
                    let _ = tx.send(Ok(()));
                    if let Err(e) = child.wait() {
                        log::warn!("custom-url-handler {cmd:?}: could not reap child: {e}");
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e));
                }
            }
        })?;
    rx.recv().map_err(std::io::Error::other)?
}

/// Open a checked, resolved local file: through `handler` with a file URL
/// built from the resolved path, falling back to the system opener, which
/// takes the path itself. Neither sees the link's own text, so neither can
/// read a different path out of it.
fn open_checked_file(resolved: &std::path::Path, handler: Option<&str>) {
    if let (Some(cmd), Some(url)) = (handler, kettle_core::links::file_url_for_path(resolved)) {
        match spawn_custom_url_handler(cmd, &url) {
            Ok(_) => return,
            Err(e) => {
                log::warn!("custom-url-handler {cmd:?}: {e}; opening the file with the system")
            }
        }
    }
    if let Err(e) = open::that_detached(resolved) {
        log::warn!("failed to open a terminal-supplied file link: {e}");
    }
}

/// Record named keys and modifiers; redact printable text unless raw input is enabled.
/// Keystroke counts and timing remain visible.
fn dev_record_key(
    rec: &mut crate::dev_record::Recorder,
    key: &winit::keyboard::Key,
    mods: ModifiersState,
) {
    use winit::keyboard::Key;
    let mut prefix = String::new();
    if mods.control_key() {
        prefix.push_str("Ctrl+");
    }
    if mods.alt_key() {
        prefix.push_str("Alt+");
    }
    if mods.super_key() {
        prefix.push_str("Super+");
    }
    let token = match key {
        Key::Named(nk) => Some(format!("{prefix}{nk:?}")),
        // AltGr can report Ctrl+Alt; redact its printable payload too.
        Key::Character(s) if !prefix.is_empty() => Some(format!(
            "{prefix}{}",
            crate::dev_record::printable_token(s.as_str(), rec.raw_input())
        )),
        Key::Character(s) => Some(crate::dev_record::printable_token(
            s.as_str(),
            rec.raw_input(),
        )),
        _ => None,
    };
    if let Some(t) = token {
        rec.record_input(&t);
    }
}

/// Neutralize an OSC-set window/tab title before it reaches
/// the OS titlebar, the tab label, and the status bar. Replaces control
/// characters AND Unicode bidirectional-override format characters (the U+202E
/// titlebar / Alt-Tab spoofing vector) with a space, and caps the length so a
/// pathological program can't set a multi-kilobyte title. Mirrors the
/// notification path's control-stripping + length-cap policy.
fn sanitize_title(t: &str) -> String {
    const MAX_TITLE_CHARS: usize = 512;
    t.chars()
        .map(|c| {
            if c.is_control() || is_bidi_format_char(c) {
                ' '
            } else {
                c
            }
        })
        .take(MAX_TITLE_CHARS)
        .collect()
}

/// Native window-manager titlebars use the desktop UI font, not kettle's
/// terminal font stack. Strip Unicode Private Use glyphs (Nerd Font icons,
/// Powerline variants, etc.) from the OS title only so GNOME/Ubuntu titlebars
/// do not show tofu boxes while in-app tabs/pane titlebars can still render
/// those glyphs through kettle's embedded font path.
fn sanitize_native_window_title(t: &str) -> String {
    let out = t
        .chars()
        .filter(|&c| !is_private_use_char(c))
        .collect::<String>();
    let out = out.trim();
    if out.is_empty() {
        "kettle".to_string()
    } else {
        out.to_string()
    }
}

fn is_private_use_char(c: char) -> bool {
    matches!(
        c,
        '\u{E000}'..='\u{F8FF}' | '\u{F0000}'..='\u{FFFFD}' | '\u{100000}'..='\u{10FFFD}'
    )
}

/// Unicode bidirectional formatting / override code points. These are NOT
/// `char::is_control()` (they are general category Cf) but enable right-to-left
/// titlebar / Alt-Tab spoofing, so a sanitized title must neutralize them too.
pub(crate) fn is_bidi_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{202A}'..='\u{202E}'      // LRE RLE PDF LRO RLO
            | '\u{2066}'..='\u{2069}' // LRI RLI FSI PDI
            | '\u{200E}' | '\u{200F}' // LRM RLM
    )
}

/// Pure pointer → `(col, line, side)` math for a pane, shared by `px_to_point` so
/// the per-pane-titlebar inset is drift-tested.
///
/// The renderer and hit testing share [`kettle_render::pane_grid_origin`] (a top
/// titlebar moves row zero down; a bottom one leaves it at the pane padding), so
/// selection, link targeting, mouse reporting, and IME projection never drift a
/// row from the drawn grid. Col/line clamp to ≥ 0 so a click in the
/// chrome/padding doesn't underflow.
///
/// `side` is which half of the hit cell the pointer sits in, derived from the
/// sub-cell x offset: the left half is `Side::Left`, the right half (and the
/// exact midpoint) is `Side::Right`. This matches xterm / Alacritty / iTerm2 and
/// is exactly what alacritty's `Selection::to_range` (`range_simple`/`range_block`)
/// needs to decide whether each boundary cell is included. Without it a drag is
/// biased one cell wide. The side is computed from the SAME clamped non-negative
/// offset as `col`, so a pointer left of the content origin maps to
/// `(col 0, Side::Left)` and the first cell is included, never trimmed. NOTE:
/// only Simple/Block *drag* selections consume this side; word / line /
/// smart-select snap to token boundaries and ignore it (see `begin_selection` /
/// `apply_smart_selection`).
fn px_to_cell(
    px: f32,
    py: f32,
    terminal: Rect,
    cell: (f32, f32),
    pad: (f32, f32),
) -> (usize, i32, kettle_core::Side) {
    let (cw, ch) = cell;
    let (ox, oy) = (terminal.0 + pad.0, terminal.1 + pad.1);
    // Derive BOTH col and side from the same non-negative offset so they agree at
    // the left clamp: a pointer in the left padding (offset < 0) maps to
    // (col 0, Side::Left) — the first cell is included, not trimmed. Computing the
    // side from the raw (negative) offset via `rem_euclid` would wrap it into the
    // cell's right half and wrongly drop column 0 from a drag.
    let offx = (px - ox).max(0.0);
    let col = (offx / cw).floor() as usize;
    let line = ((py - oy) / ch).floor().max(0.0) as i32;
    let side = if offx.rem_euclid(cw) < cw / 2.0 {
        kettle_core::Side::Left
    } else {
        kettle_core::Side::Right
    };
    (col, line, side)
}

/// Map a VIEWPORT-relative point (line 0 = top visible row, what `px_to_point`
/// returns) to the GRID-ABSOLUTE point that alacritty's `Selection` /
/// `selection_to_string` / `to_range` and `grid[..]` indexing expect, by
/// subtracting the focused pane's `display_offset` via alacritty's own
/// `viewport_to_point`. Without it, a selection or grid-row read taken while
/// scrolled into history addresses the active-screen row instead of the
/// scrolled-to row, so the copy returns wrong or empty text and the highlight
/// slips down by the scroll amount. The bug is invisible at the bottom, where
/// `display_offset == 0` makes viewport and absolute coincide. Pure
/// (drift-tested in `viewport_point_to_grid_applies_display_offset`).
fn viewport_point_to_grid(
    viewport: kettle_core::Point,
    display_offset: usize,
) -> kettle_core::Point {
    let line = viewport.line.0.max(0) as usize;
    kettle_core::viewport_to_point(
        display_offset,
        kettle_core::Point::new(line, viewport.column),
    )
}

/// Grid-absolute `(top-left, bottom-right)` points spanning the whole buffer —
/// scrollback included — for keyboard "select to top/bottom" and "select all".
/// Alacritty's convention: the active screen is `Line(0)..Line(screen_lines)` and
/// history is *negative* down to `Line(-history_size)`, so the oldest line is
/// `Line(-(history_size))` col 0 and the last cell is `Line(screen_lines-1)` at the
/// final column. Pure so the boundary math is unit-testable without a GPU/Term.
fn selection_buffer_bounds(
    history_size: usize,
    screen_lines: usize,
    columns: usize,
) -> (kettle_core::Point, kettle_core::Point) {
    let top = kettle_core::Point::new(
        kettle_core::Line(-(history_size as i32)),
        kettle_core::Column(0),
    );
    let bottom = kettle_core::Point::new(
        kettle_core::Line(screen_lines.saturating_sub(1) as i32),
        kettle_core::Column(columns.saturating_sub(1)),
    );
    (top, bottom)
}

/// Fallback output frame budget when the platform cannot identify the current
/// monitor's refresh rate. This is the ceiling-rounded period of a 60 Hz
/// display; a flat 16 ms would schedule slightly faster than a 60 Hz
/// compositor can present.
const OUTPUT_FRAME_BUDGET: std::time::Duration = std::time::Duration::from_nanos(16_666_667);
const MIN_OUTPUT_FRAME_BUDGET: std::time::Duration = std::time::Duration::from_millis(4);
const MAX_OUTPUT_FRAME_BUDGET: std::time::Duration = std::time::Duration::from_nanos(33_333_334);
/// Small dispatch/preparation lead so a deadline does not first become runnable
/// exactly at vblank and miss the compositor's next present opportunity.
/// Deliberately sub-millisecond; comparative live validation still decides
/// whether a backend/display needs a different policy.
const OUTPUT_PRESENT_HEADROOM: std::time::Duration = std::time::Duration::from_micros(250);
const MONITOR_REFRESH_PROBE_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);
const MID_FLOOD_MIN_BUDGET: std::time::Duration = std::time::Duration::from_nanos(16_666_667);
const MID_FLOOD_MAX_BUDGET: std::time::Duration = std::time::Duration::from_nanos(33_333_334);
const DEEP_FLOOD_MIN_BUDGET: std::time::Duration = std::time::Duration::from_nanos(33_333_334);
const DEEP_FLOOD_MAX_BUDGET: std::time::Duration = std::time::Duration::from_millis(50);

/// Convert winit's monitor refresh rate (millihertz) into a per-window output
/// deadline one small dispatch lead before the frame period. The 4–33.33 ms
/// bounds cover 30–250 Hz displays while
/// preventing an implausible/driver-corrupt mode from creating a busy loop or
/// making output feel stalled. A window updates this whenever it is created,
/// moved, resized, or changes DPI, so two Kettle windows on different monitors
/// can pace independently.
fn output_frame_budget_for_refresh(refresh_millihertz: Option<u32>) -> std::time::Duration {
    let Some(refresh_millihertz) = refresh_millihertz.filter(|rate| *rate > 0) else {
        return OUTPUT_FRAME_BUDGET;
    };
    const NANOS_PER_MILLIHERTZ_PERIOD: u64 = 1_000_000_000_000;
    let nanos = NANOS_PER_MILLIHERTZ_PERIOD.div_ceil(u64::from(refresh_millihertz));
    std::time::Duration::from_nanos(nanos)
        .saturating_sub(OUTPUT_PRESENT_HEADROOM)
        .clamp(MIN_OUTPUT_FRAME_BUDGET, MAX_OUTPUT_FRAME_BUDGET)
}

fn monitor_refresh_probe_wait(
    last_probe: Option<std::time::Instant>,
    now: std::time::Instant,
    force: bool,
) -> Option<std::time::Duration> {
    if force {
        return None;
    }
    last_probe
        .map(|last| {
            MONITOR_REFRESH_PROBE_INTERVAL.saturating_sub(now.saturating_duration_since(last))
        })
        .filter(|remaining| !remaining.is_zero())
}

/// Whether an output-driven repaint should be DEFERRED
/// (coalesced) rather than painted now — true when the previous frame painted
/// less than `budget` ago. Capping PTY-output paints to one per budget lets a
/// non-atomic repaint burst (an app that doesn't bracket frames with DEC 2026
/// synchronized output, e.g. Claude Code) settle before kettle snapshots the
/// grid, avoiding the transient mid-repaint cursor jump. Pure (drift-tested in
/// `output_paint_coalesces_within_frame_budget`).
fn should_defer_output_paint(
    now: std::time::Instant,
    last_paint: Option<std::time::Instant>,
    budget: std::time::Duration,
) -> bool {
    match last_paint {
        Some(t) => now.saturating_duration_since(t) < budget,
        None => false,
    }
}

/// The output-paint budget GROWS under a sustained flood.
/// Each output-driven frame that had to be coalesced — i.e. output arriving
/// faster than the monitor-derived base budget — bumps the window's
/// `flood_paints` counter. A high-refresh window steps down no further than
/// 60 fps and then 30 fps; a 60 Hz window retains the established 30/20 fps
/// backoff. On-screen content during a flood is unreadable scrolling anyway,
/// and every frame NOT painted is one fewer O(cells) `PaneSnapshot::capture`
/// taken under the pane's `Term` mutex — the SAME mutex the PTY reader thread
/// must hold to run `Processor::advance`. At 60 fps the main thread grabs that
/// lock ~60×/s under flood, throttling the parser on a CPU-contended box;
/// stretching the budget hands the lock (and the cores) back to the reader, so
/// flood throughput rises. A brief burst (< 4 coalesced frames) never
/// throttles, so keystroke echo and short bursts stay at the monitor rate, and the
/// counter resets the instant output drops below the budget (see `redraw`), so
/// the settled post-flood frame paints within one budget. Pure; drift-tested in
/// `effective_output_budget_grows_under_sustained_flood`.
fn effective_output_budget(base: std::time::Duration, flood_paints: u32) -> std::time::Duration {
    match flood_paints {
        0..=3 => base,
        4..=15 => base
            .saturating_mul(2)
            .clamp(MID_FLOOD_MIN_BUDGET, MID_FLOOD_MAX_BUDGET),
        _ => base
            .saturating_mul(3)
            .clamp(DEEP_FLOOD_MIN_BUDGET, DEEP_FLOOD_MAX_BUDGET),
    }
}

/// Output that lands within this window of a keystroke is ECHO and paints
/// immediately, skipping the coalescer, so key repeat doesn't stutter. Long
/// enough to bridge OS key-repeat intervals (~33ms at default rates) plus
/// ConPTY echo latency; short enough that an unrelated burst (a build log
/// kicking in a beat after you pressed Enter twice) re-enters the coalescer
/// quickly.
const TYPING_ECHO_WINDOW: std::time::Duration = std::time::Duration::from_millis(150);

/// Pure (drift-tested in `typed_echo_bypasses_the_output_coalescer`): is
/// `now` inside the typing-echo window after the last keystroke?
fn typed_recently(
    now: std::time::Instant,
    last_typed: Option<std::time::Instant>,
    window: std::time::Duration,
) -> bool {
    last_typed.is_some_and(|t| now.saturating_duration_since(t) < window)
}

fn pane_snapshot_keys_match(
    cached: &[(u64, u64, usize, usize)],
    current: impl IntoIterator<Item = (u64, u64, usize, usize)>,
) -> bool {
    let mut current = current.into_iter();
    for cached_key in cached {
        match current.next() {
            Some(current_key) if *cached_key == current_key => {}
            _ => return false,
        }
    }
    current.next().is_none()
}

fn cached_pane_cursor_blinking(
    active_pane: Option<u64>,
    keys: &[(u64, u64, usize, usize)],
    snapshots: &[PaneSnapshot],
) -> Option<bool> {
    let active_pane = active_pane?;
    keys.iter()
        .position(|(pane_id, ..)| *pane_id == active_pane)
        .and_then(|index| snapshots.get(index))
        .map(|snapshot| snapshot.cursor_blinking)
}

fn pane_cursor_blinking_with<F>(
    active_pane: Option<u64>,
    keys: &[(u64, u64, usize, usize)],
    snapshots: &[PaneSnapshot],
    use_pane_snapshots: bool,
    live: F,
) -> bool
where
    F: FnOnce(u64) -> Option<bool>,
{
    let Some(active_pane) = active_pane else {
        return true;
    };
    if use_pane_snapshots {
        return cached_pane_cursor_blinking(Some(active_pane), keys, snapshots).unwrap_or(true);
    }
    live(active_pane).unwrap_or(true)
}

/// Snapshot reuse is safe only when the context menu owns the pointer.
///
/// These gesture states can mutate terminal selection, scrollback, or layout
/// from a `CursorMoved` event. Cursor movement is intentionally allowed to
/// preserve the one-shot menu-hover hint, so an active gesture must block the
/// fast path even when output generation and grid dimensions still match.
fn context_menu_snapshot_reuse_safe(ws: &WindowState) -> bool {
    !ws.selecting
        && ws.scrollbar_drag_offset.is_none()
        && ws.dragging_split.is_none()
        && !ws.held_buttons.any()
        && !ws.tab_drag_active
        && ws.tab_drag_press.is_none()
        && ws.drag_press.is_none()
        && ws.pane_drag.is_none()
        && matches!(&ws.detach_drag, crate::detach::DragState::Idle)
}

/// Pure cols/rows-that-fit math for a terminal rect, shared by `grid_of`. The
/// terminal is already clear of the pane's titlebar and lane, so only the
/// padding comes off. `max(1)` keeps a degenerate tiny pane at ≥ 1×1.
fn grid_dims_px(size: (f32, f32), cell: (f32, f32), pad: (f32, f32)) -> (usize, usize) {
    let (w, h) = size;
    let (cw, ch) = cell;
    let (pad_x, pad_y) = pad;
    let cols = ((w - pad_x * 2.0) / cw).floor().max(1.0) as usize;
    let rows = ((h - pad_y * 2.0) / ch).floor().max(1.0) as usize;
    (cols, rows)
}

/// Round a fractional renderer cell metric only after multiplying by the
/// complete grid dimension. ConPTY and Unix `winsize` pixel fields are `u16`,
/// but truncating each cell first compounds the error across every row/column
/// (9.6 px × 100 columns must report 960 px, not 900 px).
fn pty_pixel_extent(cell: f32, count: usize) -> u16 {
    let cell = if cell.is_finite() {
        f64::from(cell.max(1.0))
    } else {
        1.0
    };
    let count = count.min(u16::MAX as usize) as f64;
    (cell * count).round().clamp(1.0, f64::from(u16::MAX)) as u16
}

fn pty_pixel_size(cell: (f32, f32), grid: (usize, usize)) -> (u16, u16) {
    (
        pty_pixel_extent(cell.0, grid.0),
        pty_pixel_extent(cell.1, grid.1),
    )
}

const STARTUP_GEOMETRY_CELL_W: f64 = 8.0;
const STARTUP_GEOMETRY_CELL_H: f64 = 16.0;
const STARTUP_GEOMETRY_TAB_BAR_H: f64 = 24.0;
const STARTUP_GEOMETRY_STATUS_BAR_H: f64 = 22.0;
/// The grid a fresh window aims for when the config sets no size, and the
/// value a half-specified `window-width`/`window-height` fills its other axis
/// from. Sized for agent TUIs: Claude Code's fullscreen diff panel toggles at
/// 110 columns and auto-opens at 144, and Codex CLI lays out two columns
/// above ~120, so the classic 80x24 (or winit's ~800x600, 93 columns at the
/// default font) left every fresh window too narrow for the tools Kettle
/// targets. 160x45 baseline cells are 1296x760 logical px with the default
/// chrome: about 152x41 real cells at the 14 px font on a 1080p monitor.
pub(crate) const STARTUP_DEFAULT_COLS: u32 = 160;
pub(crate) const STARTUP_DEFAULT_ROWS: u32 = 45;
/// The default window never covers more of the monitor than this: a fixed
/// target rather than a monitor fraction keeps a 3440-wide ultrawide from
/// opening a 285-column canvas, and the fit keeps a 1366x768 laptop honest
/// (its 90 % is 1229 px, about 144 columns at the default font). There is no
/// floor on purpose: a monitor smaller than the target wins.
const STARTUP_MONITOR_WIDTH_FRACTION: f64 = 0.90;
const STARTUP_MONITOR_HEIGHT_FRACTION: f64 = 0.85;

/// Convert a startup grid into an inner-window size in **logical** pixels.
/// This runs before renderer/font metrics exist, so it intentionally uses the
/// same conservative 8x16 baseline as `geometry-hinting`; the first normal
/// layout pass reconciles exact metrics after the renderer starts. Logical
/// rather than physical so a HiDPI display gets the same grid as a 1x one; a
/// physical conversion would give a 2x display half the requested columns.
fn startup_cells_to_logical_px(cfg: &Config, cols: u32, rows: u32) -> (f64, f64) {
    let cols = cols.clamp(
        kettle_config::WINDOW_WIDTH_MIN,
        kettle_config::WINDOW_WIDTH_MAX,
    );
    let rows = rows.clamp(
        kettle_config::WINDOW_HEIGHT_MIN,
        kettle_config::WINDOW_HEIGHT_MAX,
    );

    let mut width = f64::from(cols) * STARTUP_GEOMETRY_CELL_W + f64::from(cfg.padding_x) * 2.0;
    let mut height = f64::from(rows) * STARTUP_GEOMETRY_CELL_H + f64::from(cfg.padding_y) * 2.0;

    if matches!(cfg.tab_bar, TabBarMode::Always) {
        match cfg.tab_bar_pos {
            TabBarPos::Top | TabBarPos::Bottom => height += STARTUP_GEOMETRY_TAB_BAR_H,
            TabBarPos::Left | TabBarPos::Right => {
                width += f64::from(cfg.tab_bar_width.clamp(40.0, 600.0));
            }
        }
    }

    if !matches!(cfg.status_bar, StatusBarMode::Off) {
        height += STARTUP_GEOMETRY_STATUS_BAR_H;
    }

    (width.ceil().max(1.0), height.ceil().max(1.0))
}

/// The inner size an explicit `window-width` / `window-height` asks for, in
/// logical pixels, or `None` when the config sets neither. A missing axis
/// comes from the default grid so `window-width = 200` alone still opens
/// tall enough for an agent TUI. Explicit values are honoured as typed: the
/// monitor fit below applies to the default only.
fn startup_inner_size_px(cfg: &Config) -> Option<(u32, u32)> {
    if cfg.window_width.is_none() && cfg.window_height.is_none() {
        return None;
    }
    let (width, height) = startup_cells_to_logical_px(
        cfg,
        cfg.window_width.unwrap_or(STARTUP_DEFAULT_COLS),
        cfg.window_height.unwrap_or(STARTUP_DEFAULT_ROWS),
    );
    Some((width as u32, height as u32))
}

/// The fresh-window default when the config sets no size: the default grid
/// with this config's chrome, fitted to the monitor it will most likely open
/// on. `monitor_logical` is the monitor's logical size; `None` (no monitor
/// enumerable yet) keeps the bare target.
fn default_startup_inner_size(cfg: &Config, monitor_logical: Option<(f64, f64)>) -> (f64, f64) {
    let (width, height) =
        startup_cells_to_logical_px(cfg, STARTUP_DEFAULT_COLS, STARTUP_DEFAULT_ROWS);
    let Some((monitor_w, monitor_h)) = monitor_logical else {
        return (width, height);
    };
    let max_w = (monitor_w * STARTUP_MONITOR_WIDTH_FRACTION)
        .floor()
        .max(1.0);
    let max_h = (monitor_h * STARTUP_MONITOR_HEIGHT_FRACTION)
        .floor()
        .max(1.0);
    (width.min(max_w), height.min(max_h))
}

/// The one startup size both window constructors use, in logical pixels:
/// the configured grid when the user set one, else the monitor-fitted default.
fn startup_inner_size(cfg: &Config, monitor: Option<StartupMonitor>) -> (f64, f64) {
    match startup_inner_size_px(cfg) {
        Some((w, h)) => (f64::from(w), f64::from(h)),
        None => default_startup_inner_size(cfg, monitor.map(|m| m.logical)),
    }
}

/// The surface size, in physical pixels, whose content area holds exactly
/// `grid` cells of `cell` inside `pad` and the chrome `bands` describe.
///
/// `content_rect_for_with_strip` subtracts fixed bands, so their total is read
/// off a probe surface and added back. Half a pixel of slack before rounding
/// up keeps float error from costing a column; the slack stays under a cell.
fn surface_for_grid(
    grid: (usize, usize),
    cell: (f32, f32),
    pad: (f32, f32),
    bands: ContentBands,
    tab_bar_pos: kettle_config::TabBarPos,
    status_bar: kettle_config::StatusBarMode,
    strip_w: f32,
) -> (u32, u32) {
    const PROBE: u32 = 1 << 16;
    let (_, _, content_w, content_h) =
        content_rect_for_with_strip((PROBE, PROBE), bands, tab_bar_pos, status_bar, strip_w);
    let chrome_w = PROBE as f32 - content_w;
    let chrome_h = PROBE as f32 - content_h;
    let width = grid.0 as f32 * cell.0 + pad.0 * 2.0 + chrome_w;
    let height = grid.1 as f32 * cell.1 + pad.1 * 2.0 + chrome_h;
    (
        (width + 0.5).ceil().max(1.0) as u32,
        (height + 0.5).ceil().max(1.0) as u32,
    )
}

/// What the startup sizing needs to know about the monitor a fresh window
/// will most likely land on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StartupMonitor {
    /// Logical size (physical pixels divided by the scale factor).
    logical: (f64, f64),
    scale: f64,
}

impl StartupMonitor {
    /// CoreGraphics points converted exactly as winit's macOS monitor size.
    #[cfg(any(target_os = "macos", test))]
    pub(crate) fn from_display_points(width: usize, height: usize, scale: f64) -> Self {
        let physical = winit::dpi::PhysicalSize::<u32>::from_logical::<_, f64>(
            (width as f64, height as f64),
            scale,
        );
        Self::from_physical(physical.width, physical.height, scale)
    }

    fn from_physical(width: u32, height: u32, scale: f64) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        Self {
            logical: (f64::from(width) / scale, f64::from(height) / scale),
            scale,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct StartupPlan {
    launch_override: bool,
    may_restore: bool,
    pre_launch: bool,
}

fn startup_plan(
    options: &crate::Options,
    cfg: &Config,
    another_build_running: bool,
) -> StartupPlan {
    let launch_override = options.command.is_some() || options.cwd.is_some();
    let may_restore = !launch_override
        && (options.tab_handoff_fd.is_some()
            || options.tab_handoff.is_some()
            || options.layout.is_some()
            || should_restore_session(options.restore, cfg.restore_session, another_build_running));
    StartupPlan {
        launch_override,
        may_restore,
        pre_launch: !may_restore
            && matches!(
                cfg.window_state,
                kettle_config::WindowState::Normal | kettle_config::WindowState::Hidden
            )
            && cfg.window_position_x.is_none()
            && cfg.window_position_y.is_none(),
    }
}

fn fits_monitor(surface: (u32, u32), monitor: Option<StartupMonitor>) -> bool {
    let (width, height) = surface;
    monitor.is_some_and(|m| {
        f64::from(width) <= (m.logical.0 * STARTUP_MONITOR_WIDTH_FRACTION).floor() * m.scale + 1.0
            && f64::from(height)
                <= (m.logical.1 * STARTUP_MONITOR_HEIGHT_FRACTION).floor() * m.scale + 1.0
    })
}

/// The monitor to fit a fresh window to: the primary monitor where the
/// platform has one, else the largest by area (Wayland has no primary; a
/// mixed multi-monitor setup then gets a deterministic answer instead of
/// whichever output the compositor listed first).
fn startup_monitor(event_loop: &ActiveEventLoop) -> Option<StartupMonitor> {
    let describe = |m: winit::monitor::MonitorHandle| {
        let size = m.size();
        StartupMonitor::from_physical(size.width, size.height, m.scale_factor())
    };
    event_loop.primary_monitor().map(describe).or_else(|| {
        event_loop
            .available_monitors()
            .map(describe)
            .max_by(|a, b| {
                let area = |m: &StartupMonitor| m.logical.0 * m.logical.1;
                area(a).total_cmp(&area(b))
            })
    })
}

#[cfg(test)]
fn cursor_in_tab_bar_band(y: f32, bar_h: f32, surface_h: f32, pos: TabBarPos) -> bool {
    if bar_h <= 0.0 {
        return false;
    }
    // Left/Right strips span the full window height, so every in-window y is in
    // the band. `cursor_in_tab_bar` checks x against the strip's edge.
    match pos {
        TabBarPos::Top => y >= 0.0 && y < bar_h,
        TabBarPos::Bottom => y >= (surface_h - bar_h) && y <= surface_h,
        TabBarPos::Left | TabBarPos::Right => y >= 0.0 && y <= surface_h,
    }
}

fn status_bar_y(
    surface_h: f32,
    bar_h: f32,
    pos: kettle_config::StatusBarMode,
    tab_top_h: f32,
    search_h: f32,
) -> f32 {
    match pos {
        kettle_config::StatusBarMode::Top => tab_top_h,
        kettle_config::StatusBarMode::Bottom => surface_h - search_h - bar_h,
        kettle_config::StatusBarMode::Off => 0.0,
    }
}

/// Sibling of `cursor_in_tab_bar_band` for the status bar, so the
/// chrome-cursor logic treats both bars uniformly. The status strip is outside
/// every pane rect and the tab-bar band, so without this check hovering it
/// falls through to the "over a pane" branch and shows the I-beam.
fn cursor_in_status_bar_band(
    y: f32,
    bar_h: f32,
    surface_h: f32,
    pos: kettle_config::StatusBarMode,
    tab_top_h: f32,
    search_h: f32,
) -> bool {
    if bar_h <= 0.0 {
        return false;
    }
    let bar_y = status_bar_y(surface_h, bar_h, pos, tab_top_h, search_h);
    match pos {
        kettle_config::StatusBarMode::Off => false,
        kettle_config::StatusBarMode::Top => y >= bar_y && y < bar_y + bar_h,
        kettle_config::StatusBarMode::Bottom => y >= bar_y && y <= bar_y + bar_h,
    }
}

/// Auto-scroll rate when a selection reaches or passes a pane's vertical edge.
/// A six-point inner drag zone keeps full-height panes scrollable even though
/// the pointer cannot move below their client area.
///
/// Speed scales with how far past the edge the cursor sits — a small
/// overshoot crawls (1 line/frame), a big one (40+ px) chases at 3 lines
/// per frame. Pure so the cadence is unit-tested without spinning up a
/// renderer or PTY.
fn selection_autoscroll_lines(
    y: f32,
    rect_top: f32,
    rect_bottom: f32,
    scale_factor: f32,
    selection_dragged: bool,
) -> i32 {
    const EDGE_ZONE_PT: f32 = 6.0;

    let pane_height = rect_bottom - rect_top;
    if !selection_dragged || pane_height <= 0.0 {
        return 0;
    }
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let edge_zone = (EDGE_ZONE_PT * scale_factor).min(pane_height * 0.25);
    let (overshoot, direction) = if y < rect_top || y <= rect_top + edge_zone {
        ((rect_top - y).max(0.0), 1)
    } else if y > rect_bottom || y >= rect_bottom - edge_zone {
        ((y - rect_bottom).max(0.0), -1)
    } else {
        return 0;
    };
    // Magnitude → 1..=3 lines/frame ladder.
    let mag = if overshoot >= 40.0 {
        3
    } else if overshoot >= 10.0 {
        2
    } else {
        1
    };
    direction * mag
}

/// Whether pointer travel since the selection press is large enough to count
/// as a drag. Native backends may emit a cursor-moved event after mouse-down at
/// the same (or fractionally rounded) coordinate, so event arrival alone is
/// not evidence that the user dragged. The threshold is in logical points and
/// converted to physical pixels for the addressed window.
fn selection_drag_motion_started(
    origin: Option<winit::dpi::PhysicalPosition<f64>>,
    cursor: winit::dpi::PhysicalPosition<f64>,
    scale_factor: f64,
) -> bool {
    const DRAG_START_PT: f64 = 2.0;

    let Some(origin) = origin else {
        return false;
    };
    if !(origin.x.is_finite()
        && origin.y.is_finite()
        && cursor.x.is_finite()
        && cursor.y.is_finite())
    {
        return false;
    }
    let scale_factor = if scale_factor.is_finite() && scale_factor > 0.0 {
        scale_factor
    } else {
        1.0
    };
    let threshold = DRAG_START_PT * scale_factor;
    let dx = cursor.x - origin.x;
    let dy = cursor.y - origin.y;
    dx * dx + dy * dy >= threshold * threshold
}

fn note_selection_drag_motion(ws: &mut WindowState) {
    if ws.selection_dragged {
        return;
    }
    let scale_factor = ws
        .window
        .as_ref()
        .map_or(1.0, |window| window.scale_factor());
    ws.selection_dragged =
        selection_drag_motion_started(ws.selection_drag_origin, ws.cursor, scale_factor);
}

/// Which vertical edge an in-progress selection most likely crossed when the
/// backend reports `CursorLeft` without an out-of-client coordinate. Corners
/// resolve vertically on ties because horizontal selection overflow does not
/// scroll, while losing the vertical intent freezes the gesture entirely. A
/// window exit only latches after measured drag travel; otherwise a press on
/// the last client pixel plus sub-threshold outward jitter could scroll.
fn selection_leave_edge(selection_dragged: bool, x: f32, y: f32, width: f32, height: f32) -> i8 {
    if !selection_dragged || !(x.is_finite() && y.is_finite() && width > 0.0 && height > 0.0) {
        return 0;
    }
    let top = y.max(0.0);
    let bottom = (height - y).max(0.0);
    let left = x.max(0.0);
    let right = (width - x).max(0.0);
    if top <= bottom && top <= left && top <= right {
        1
    } else if bottom <= top && bottom <= left && bottom <= right {
        -1
    } else {
        0
    }
}

fn arm_selection_gesture(ws: &mut WindowState, pane_id: u64, button: u8) {
    // A fresh grid gesture owns Copy, including Shift+right-click extension.
    if ws.search.open {
        ws.search.editor.clear_selection();
    }
    ws.selecting = true;
    ws.selecting_pane = Some(pane_id);
    ws.selection_autoscroll_edge = 0;
    ws.selection_dragged = false;
    ws.selection_drag_origin = Some(ws.cursor);
    ws.selection_button = Some(button);
}

fn selection_release_matches(owner: Option<u8>, button: u8) -> bool {
    owner == Some(button)
}

fn selection_copy_owner(selecting: bool, pane_id: Option<u64>) -> Option<u64> {
    if selecting { pane_id } else { None }
}

fn clear_selection_gesture(ws: &mut WindowState) {
    ws.selecting = false;
    ws.selecting_pane = None;
    ws.selection_autoscroll_edge = 0;
    ws.selection_dragged = false;
    ws.selection_drag_origin = None;
    ws.selection_button = None;
}

fn take_window_close_request(pending: &mut std::collections::BTreeSet<u64>, seq: u64) -> bool {
    pending.remove(&seq)
}

/// Select the terminal pane addressed by a wheel event without mutating
/// keyboard focus. A pane under the pointer wins; chrome, gaps and coordinates
/// outside the client area fall back to the focused pane.
fn wheel_target_at(
    layout: &[(u64, Rect)],
    focused: Option<u64>,
    x: f32,
    y: f32,
) -> Option<(u64, Rect)> {
    layout
        .iter()
        .copied()
        .find(|&(_, (rx, ry, rw, rh))| x >= rx && x < rx + rw && y >= ry && y < ry + rh)
        .or_else(|| {
            let focused = focused?;
            layout.iter().copied().find(|(pane, _)| *pane == focused)
        })
}

/// Render the OS window title from the user's `window-title-format`
/// template with the active pane's title / cwd / 1-based tab index. While
/// the pane title is still the initial placeholder ("kettle") and the cwd
/// is known, substitute the cwd's basename so `~/Repos/kettle` shows up
/// as `kettle — kettle` and `~/Documents` as `Documents — kettle`,
/// matching the cwd-basename tab-title fallback so the OS window title and
/// the in-app tab agree pre-OSC 2. Real shell-set titles still win the
/// moment they arrive.
fn window_title(template: &str, pane_title: &str, cwd: &str, tab: usize) -> String {
    let home = crate::mux::home_dir_string();
    window_title_with_home(template, pane_title, cwd, home.as_deref(), tab)
}

/// Expand a leading `~` / `~/…` in a config-file path to the user's home dir.
/// Config values (unlike `--record-dir` / `KETTLE_RECORD_DIR`, which a shell
/// expands before Kettle sees them) are never shell-expanded, so a literal
/// `record-dir = ~/.cache/kettle/records` would otherwise create a `~`-named
/// directory under the launch CWD. Falls back to the raw path if `HOME` /
/// `USERPROFILE` is unset.
fn expand_user_path(path: &std::path::Path) -> std::path::PathBuf {
    expand_user_path_with(path, crate::mux::home_dir_string())
}

/// Pure core of [`expand_user_path`], parameterized on the home dir for testing.
fn expand_user_path_with(path: &std::path::Path, home: Option<String>) -> std::path::PathBuf {
    let Some(s) = path.to_str() else {
        return path.to_path_buf();
    };
    if (s == "~" || s.starts_with("~/"))
        && let Some(home) = home.filter(|h| !h.is_empty())
    {
        let mut expanded = std::path::PathBuf::from(home);
        if let Some(rest) = s.strip_prefix("~/") {
            expanded.push(rest);
        }
        return expanded;
    }
    path.to_path_buf()
}

const RECORDING_TITLE_SUFFIX: &str = " [REC]";
// A distinct suffix while RAW keystroke capture is active, so literal-keystroke
// (potentially password-capturing) recording is never visually silent.
const RECORDING_RAW_TITLE_SUFFIX: &str = " [REC RAW]";
const RECORDING_LIMIT_TITLE_SUFFIX: &str = " [REC LIMIT]";
const RECORDING_OVERLOAD_TITLE_SUFFIX: &str = " [REC INCOMPLETE]";
const RECORDING_ERROR_TITLE_SUFFIX: &str = " [REC ERROR]";

fn recording_window_title(
    mut title: String,
    status: Option<crate::dev_record::RecordStatus>,
    raw_input: bool,
) -> String {
    match status {
        Some(crate::dev_record::RecordStatus::Recording) => title.push_str(if raw_input {
            RECORDING_RAW_TITLE_SUFFIX
        } else {
            RECORDING_TITLE_SUFFIX
        }),
        Some(crate::dev_record::RecordStatus::LimitReached) => {
            title.push_str(RECORDING_LIMIT_TITLE_SUFFIX)
        }
        Some(crate::dev_record::RecordStatus::Overloaded) => {
            title.push_str(RECORDING_OVERLOAD_TITLE_SUFFIX)
        }
        Some(crate::dev_record::RecordStatus::IoError) => {
            title.push_str(RECORDING_ERROR_TITLE_SUFFIX)
        }
        None => {}
    }
    title
}

fn effective_record_status(
    recorder: Option<crate::dev_record::RecordStatus>,
    start_failed: bool,
) -> Option<crate::dev_record::RecordStatus> {
    recorder.or(start_failed.then_some(crate::dev_record::RecordStatus::IoError))
}

fn activation_recording_available(
    required: bool,
    status: Option<crate::dev_record::RecordStatus>,
) -> bool {
    !required || status == Some(crate::dev_record::RecordStatus::Recording)
}

fn window_title_with_home(
    template: &str,
    pane_title: &str,
    cwd: &str,
    home: Option<&str>,
    tab: usize,
) -> String {
    let t_raw = pane_title.trim();
    let pane_placeholder = t_raw.is_empty() || t_raw == "kettle";
    let cwd_basename = std::path::Path::new(cwd)
        .file_name()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty());
    let cwd_display = crate::mux::abbreviate_home(cwd, home);
    // Prefer the cwd basename until the shell names the pane. A directory
    // named "kettle" is real; only the pane title is a placeholder.
    if pane_placeholder {
        if cwd_basename.is_none() && template == kettle_config::DEFAULT_WINDOW_TITLE_FORMAT {
            return "kettle".to_string();
        }
        let tab = tab.to_string();
        return kettle_config::template::fill(
            template,
            &[
                ("title", cwd_basename.unwrap_or("kettle")),
                ("cwd", &cwd_display),
                ("tab", &tab),
            ],
        );
    }
    if !cwd.is_empty()
        && let Some(label) = crate::mux::cwd_label_for_shell_title(t_raw, cwd, home)
        && let Some(path) = label.path.as_deref()
    {
        let tab = tab.to_string();
        return kettle_config::template::fill(
            template,
            &[("title", path), ("cwd", &cwd_display), ("tab", &tab)],
        );
    }
    let tab = tab.to_string();
    kettle_config::template::fill(
        template,
        &[("title", t_raw), ("cwd", &cwd_display), ("tab", &tab)],
    )
}

/// Vi-mode UI ownership. Cursor, viewport following, and selection coordinates
/// live in alacritty_terminal's `Term`; that engine rotates them with output,
/// history eviction, and reflow. Keeping only the owning pane id and visual
/// state here prevents a second coordinate model from drifting.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ViState {
    pane_id: u64,
    visual: bool,
}

/// Truncate a status-bar title to `max` chars, appending `…` so the elision
/// is visible. Counts chars, not bytes, so UTF-8 multibyte glyphs aren't split.
/// A title that already fits is returned unchanged.
///
/// Without this, a long pane title wraps past the status strip's 1-cell
/// height and the rest is hidden with no indication.
fn cap_title_for_status_bar(title: &str, max: usize) -> String {
    if title.chars().count() <= max {
        return title.to_string();
    }
    let mut out: String = title.chars().take(max).collect();
    out.push('…');
    out
}

/// Peacock: a STABLE per-window seed for `accent-color = auto`,
/// hashed from the window's working directory (the launch `-d DIR`, else the
/// process cwd). Same project → same seed → same accent across launches;
/// different projects → different accents. Pure given the cwd.
fn accent_seed_from_cwd(cwd: Option<&std::path::Path>) -> u64 {
    use std::hash::{Hash, Hasher};
    let dir = cwd
        .map(|p| p.to_path_buf())
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    // Canonicalize so every spelling of the same project
    // (`-d .`, `-d C:\proj`, a relative path, a trailing slash) hashes to the
    // SAME seed — the documented "same project → same accent" stability.
    // Falls back to the raw path when it doesn't resolve (nonexistent dir).
    let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
    let mut h = std::collections::hash_map::DefaultHasher::new();
    dir.hash(&mut h);
    h.finish()
}

/// Terminator parity ("Read only"):
/// the per-pane titlebar label, composed from the pane's state badges —
/// `[RO] ` while the pane is read-only (input dropped before the PTY), then
/// the `agent-badge` when an agent control connection has the pane attached.
/// Pure (unit-tested). No badges → the title unchanged (zero cost for the
/// common case).
#[cfg(test)]
fn compose_pane_title(badge: &str, attached: bool, read_only: bool, title: &str) -> String {
    let prefix = pane_title_prefix(badge, attached, read_only);
    if prefix.is_empty() {
        title.to_string()
    } else {
        format!("{prefix}{title}")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PaneTitleParts {
    prefix: String,
    title: String,
    path: Option<String>,
}

/// The pane titlebar's mark for media on the pane's shelf that the user has
/// not looked at yet: `▣` and how many, or nothing.
fn media_shelf_badge(unseen: usize) -> String {
    if unseen == 0 {
        String::new()
    } else {
        format!("▣{unseen} ")
    }
}

fn pane_title_prefix(badge: &str, attached: bool, read_only: bool) -> String {
    let agent = attached && !badge.is_empty();
    if !read_only && !agent {
        return String::new();
    }
    let mut out = String::new();
    if read_only {
        out.push_str("[RO] ");
    }
    if agent {
        out.push_str(badge);
    }
    out
}

fn pane_title_parts(
    agent_badge: &str,
    attached: bool,
    read_only: bool,
    pane_title: &str,
    placeholder: bool,
    cwd: Option<&str>,
    home: Option<&str>,
) -> PaneTitleParts {
    let prefix = pane_title_prefix(agent_badge, attached, read_only);
    let raw = pane_title.trim();
    if let Some(cwd) = cwd.filter(|c| !c.is_empty()) {
        if placeholder || raw.is_empty() {
            let path = crate::mux::abbreviate_home(cwd, home);
            let title = cwd
                .rsplit(['/', '\\'])
                .find(|s| !s.is_empty())
                .unwrap_or(raw)
                .to_string();
            return PaneTitleParts {
                prefix,
                title,
                path: Some(path),
            };
        }
        if let Some(label) = crate::mux::cwd_label_for_shell_title(raw, cwd, home) {
            return PaneTitleParts {
                prefix,
                title: label.text,
                path: label.path,
            };
        }
    }
    PaneTitleParts {
        prefix,
        title: if raw.is_empty() {
            "kettle".to_string()
        } else {
            raw.to_string()
        },
        path: None,
    }
}

/// Does this OSC 2 title look like the bogus full-exe-path that
/// conhost/ConPTY injects at startup for a native Windows shell, rather than a
/// title the program set deliberately? When a stock `pwsh`/`cmd` launches,
/// ConPTY seeds the window title with the launched executable's absolute path
/// (e.g. `C:\Program Files\…\pwsh.exe`); kettle would otherwise show that ugly
/// path and never fall back to the working directory.
///
/// Returns true when the title is that injected launch path:
///   - with an explicit launch `argv`, the title must equal argv0 (case-
///     insensitive, `/`+`\` normalized) OR be an absolute `X:\…\<leaf>.exe`
///     whose leaf matches argv0's leaf — tight, so a program that sets its own
///     title to some other path is never mistaken for the injection;
///   - for the default shell (empty `argv`, argv0 unknown), any absolute
///     `X:\…\*.exe` path qualifies — safe because the caller only consults this
///     while the pane title is still the placeholder (the first title seen), and
///     a real shell title is virtually never a bare absolute `.exe` path.
///
/// Pure (unit-tested). Never fires on Unix/macOS — those shells don't emit an
/// absolute-Windows-path title and conhost doesn't exist there.
fn suppress_startup_title(title: &str, argv: &[String], windows: bool) -> bool {
    windows && is_conhost_startup_title(title, argv)
}

fn is_conhost_startup_title(title: &str, argv: &[String]) -> bool {
    let t = title.trim().trim_matches('"');
    if t.is_empty() {
        return false;
    }
    let tl = t.replace('/', "\\").to_ascii_lowercase();
    let looks_abs_exe = {
        let b = tl.as_bytes();
        b.len() >= 3 && b[1] == b':' && b[2] == b'\\' && tl.ends_with(".exe")
    };
    let leaf = |s: &str| {
        s.rsplit(['/', '\\'])
            .find(|p| !p.is_empty())
            .unwrap_or(s)
            .to_ascii_lowercase()
    };
    match argv.first().map(String::as_str) {
        Some(arg0) => {
            let a_n = arg0.replace('/', "\\").to_ascii_lowercase();
            tl == a_n || (looks_abs_exe && leaf(t) == leaf(arg0))
        }
        None => looks_abs_exe,
    }
}

/// Is this argv launching `ssh` (by argv0 basename)? An ssh pane has no
/// local working directory, so the native-cwd poll skips it (a native read would
/// surface kettle's launch dir, not the remote host's). OSC 7 from the remote
/// shell remains the only meaningful cwd there.
fn argv_is_ssh(argv: &[String]) -> bool {
    argv.first()
        .map(|p| {
            let last = p.rsplit(['/', '\\']).next().unwrap_or(p);
            last.eq_ignore_ascii_case("ssh") || last.eq_ignore_ascii_case("ssh.exe")
        })
        .unwrap_or(false)
}

fn update_window_remote_contexts(
    snapshot: &kettle_remote::RemoteProbeSnapshot,
    applied: &mut std::collections::HashMap<(u64, u32), kettle_remote::RemoteProbe>,
    ws: &mut WindowState,
) -> bool {
    let mut changed = false;
    for (&pane_id, pane) in &mut ws.mux.panes {
        let Some(pid) = pane.term.child_pid() else {
            continue;
        };
        let Some(probe) = snapshot.probes.get(&pid) else {
            continue;
        };
        let context_changed = applied.get(&(pane_id, pid)).is_none_or(|previous| {
            previous.remote != probe.remote
                || previous.native_cwd != probe.native_cwd
                || previous.foreground_shell != probe.foreground_shell
        });
        stage_applied_remote_probe(applied, pane_id, pid, probe);
        if context_changed {
            pane.term.set_native_cwd(probe.native_cwd.clone());
        }
        if probe.remote != pane.remote_context {
            apply_remote_title_transition(
                &mut pane.title,
                &mut pane.title_is_placeholder,
                &mut pane.title_origin,
                &mut pane.title_before_remote,
                probe.remote.as_ref(),
            );
            pane.remote_context = probe.remote.clone();
            changed = true;
        }
        if probe.foreground_process != pane.foreground_process {
            pane.foreground_process = probe.foreground_process.clone();
        }
        // Foreground argv drives input policy but has no visual representation;
        // changing commands must not schedule a title sync and redraw at 5 Hz.
        changed |= context_changed;
    }
    changed
}

/// May a shell-emitted OSC 0/2 title replace the pane's current one?
///
/// Everything except a hand-set title, which the user typed and expects to
/// stay. Shells emit OSC 0/2 on every prompt, so without this rule a manual
/// name survived less than a second.
///
/// A REMOTE title still takes precedence over a manual one, deliberately:
/// `apply_remote_title_transition` saves the previous title and restores it
/// when the remote context ends, so the manual name comes back on exit rather
/// than being lost, and while connected the host is worth showing.
fn osc_title_may_replace(origin: PaneTitleOrigin) -> bool {
    origin != PaneTitleOrigin::Manual
}

/// Apply a shell-driven title change: `Some(t)` for an OSC 0/2 set, `None`
/// for a reset.
///
/// Both doors need the same two rules, which is why they share one function:
///
/// * A `Manual` title is the user's, and the shell may not touch it by either
///   door. Shells also emit reset sequences at prompts, so gating only the set
///   would keep a hand-set name alive only until the next reset.
/// * Inside a remote context the change IS applied (the remote shell's own
///   title, `user@host:~/work`, beats kettle's synthetic label), but the origin
///   stays `Remote` and the saved pre-remote title is left alone. Overwriting
///   the saved title would lose the title to restore on disconnect. Demoting
///   the origin would stop `apply_remote_title_transition` from recognising the
///   pane as leaving a remote context, leaving the remote host's name on a
///   local pane after ssh exits.
fn apply_shell_title(
    title: &mut String,
    title_is_placeholder: &mut bool,
    origin: &mut PaneTitleOrigin,
    title_before_remote: &mut Option<(String, PaneTitleOrigin)>,
    new: Option<&str>,
) {
    if !osc_title_may_replace(*origin) {
        return;
    }
    let in_remote = *origin == PaneTitleOrigin::Remote;
    match new {
        // Sanitize before storing; this string flows to
        // set_title(), the tab label, and the status bar.
        Some(t) => {
            *title = sanitize_title(t);
            *title_is_placeholder = false;
        }
        None => {
            *title = "kettle".into();
            *title_is_placeholder = true;
        }
    }
    if in_remote {
        return;
    }
    *origin = match new {
        Some(_) => PaneTitleOrigin::Osc,
        None => PaneTitleOrigin::Placeholder,
    };
    *title_before_remote = None;
}

fn apply_remote_title_transition(
    title: &mut String,
    title_is_placeholder: &mut bool,
    origin: &mut PaneTitleOrigin,
    title_before_remote: &mut Option<(String, PaneTitleOrigin)>,
    remote: Option<&kettle_remote::RemoteContext>,
) {
    if let Some(context) = remote {
        if *origin != PaneTitleOrigin::Remote {
            *title_before_remote = Some((title.clone(), *origin));
        }
        *title = sanitize_title(&kettle_remote::format_remote_title(context));
        *title_is_placeholder = false;
        *origin = PaneTitleOrigin::Remote;
        return;
    }

    if *origin == PaneTitleOrigin::Remote {
        let (restored_title, restored_origin) = title_before_remote
            .take()
            .unwrap_or_else(|| ("kettle".to_string(), PaneTitleOrigin::Placeholder));
        *title = restored_title;
        *title_is_placeholder = restored_origin == PaneTitleOrigin::Placeholder;
        *origin = restored_origin;
    } else {
        *title_before_remote = None;
    }
}

fn retain_live_remote_probes(
    applied: &mut std::collections::HashMap<(u64, u32), kettle_remote::RemoteProbe>,
    live_panes: &mut HashSet<(u64, u32)>,
    panes: impl IntoIterator<Item = (u64, Option<u32>)>,
) {
    live_panes.clear();
    live_panes.extend(
        panes
            .into_iter()
            .filter_map(|(pane_id, pid)| pid.map(|pid| (pane_id, pid))),
    );
    applied.retain(|key, _| live_panes.contains(key));
}

fn stage_applied_remote_probe(
    applied: &mut std::collections::HashMap<(u64, u32), kettle_remote::RemoteProbe>,
    pane_id: u64,
    pid: u32,
    probe: &kettle_remote::RemoteProbe,
) -> bool {
    let key = (pane_id, pid);
    if applied.get(&key) == Some(probe) {
        false
    } else {
        applied.insert(key, probe.clone());
        true
    }
}

fn argv_is_nonlocal_client(argv: &[String]) -> bool {
    if crate::mux::argv_is_wsl(argv) || argv_is_ssh(argv) {
        return true;
    }
    let Some(argv0) = argv.first() else {
        return false;
    };
    let basename = argv0
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(argv0)
        .to_ascii_lowercase();
    let basename = basename.strip_suffix(".exe").unwrap_or(&basename);
    matches!(basename, "docker" | "podman" | "kubectl" | "lxc-attach")
}

fn remote_probe_target(pane: &crate::mux::Pane) -> Option<kettle_remote::RemoteProbeTarget> {
    #[cfg(unix)]
    let foreground_pid = pane.term.foreground_process_group().ok();
    #[cfg(not(unix))]
    let foreground_pid = None;
    Some(kettle_remote::RemoteProbeTarget {
        pid: pane.term.child_pid()?,
        foreground_pid,
        allow_native_cwd: !argv_is_nonlocal_client(&pane.argv),
    })
}

fn stage_remote_targets(
    scratch: &mut Vec<kettle_remote::RemoteProbeTarget>,
    checked_out: impl IntoIterator<Item = Option<kettle_remote::RemoteProbeTarget>>,
    stored_windows: impl IntoIterator<Item = Option<kettle_remote::RemoteProbeTarget>>,
) {
    scratch.clear();
    scratch.extend(checked_out.into_iter().flatten());
    scratch.extend(stored_windows.into_iter().flatten());
}

/// Map a click count + the Alt modifier to a selection type: double =
/// word, triple = line, single = a normal drag, and Alt+single =
/// rectangular/block selection (iTerm2/Alacritty/WezTerm parity).
fn selection_kind(clicks: u8, alt: bool) -> kettle_core::SelectionType {
    use kettle_core::SelectionType::*;
    match clicks {
        2 => Semantic,
        3 => Lines,
        _ if alt => Block,
        _ => Simple,
    }
}

/// Compile each configured `OutputTrigger`'s pattern to a
/// `regex::Regex`, log + drop invalid patterns. Pure helper so the
/// App constructor + `reload_config` path use exactly the same
/// regex set after a config edit. An empty input returns an empty
/// vec — `match_triggers` short-circuits on that.
fn compile_triggers(
    triggers: &[kettle_config::OutputTrigger],
) -> Vec<(regex::Regex, kettle_config::TriggerAction)> {
    let mut out = Vec::with_capacity(triggers.len());
    for t in triggers {
        // Multi-line: the snapshot joins viewport rows with '\n', and `^` / `$`
        // should anchor to each row.
        match regex::RegexBuilder::new(&t.pattern)
            .multi_line(true)
            .build()
        {
            Ok(re) => out.push((re, t.action.clone())),
            Err(e) => {
                log::warn!("trigger pattern {:?} failed to compile: {e}", t.pattern);
            }
        }
    }
    out
}

const CONFIG_RELOAD_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(75);

/// Should a live config reload fire a "malformed values ignored" notification?
/// Only when the diagnostics are non-empty AND differ from the set surfaced on
/// the previous reload — kettle's own settings-persistence writes (theme /
/// preferences toggles) reload the config, and an unchanged malformed set must
/// not re-notify on every one of them. Pure so the edge-trigger rule is tested.
fn should_notify_malformed(new: &[String], last: &[String]) -> bool {
    !new.is_empty() && new != last
}

/// Filesystem watchers observe the containing directory so atomic replacement
/// works across platforms. Only content/name changes to the exact file are
/// actionable. Accepting `Access(Open)` creates a read -> reload -> read
/// feedback loop on Linux inotify; accepting `Modify(Metadata)` creates the
/// same loop on macOS because opening a private state file re-applies its mode.
fn watcher_event_matches(event: &notify::Event, watched: &std::path::Path) -> bool {
    use notify::EventKind;
    use notify::event::ModifyKind;

    if !event.paths.iter().any(|path| path == watched) {
        return false;
    }
    match event.kind {
        EventKind::Any | EventKind::Create(_) | EventKind::Remove(_) => true,
        EventKind::Modify(ModifyKind::Metadata(_)) => false,
        EventKind::Modify(_) => true,
        EventKind::Access(_) | EventKind::Other => false,
    }
}

/// Queue at most one event until the main thread re-arms the latch. Re-open it
/// when the event loop has already closed so a failed send cannot strand the
/// watcher in a permanently pending state.
fn queue_watcher_event(
    pending: &std::sync::atomic::AtomicBool,
    send: impl FnOnce() -> bool,
) -> bool {
    use std::sync::atomic::Ordering;

    if pending
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }
    if send() {
        true
    } else {
        pending.store(false, Ordering::Release);
        false
    }
}

fn config_watch_paths(path: &std::path::Path) -> std::io::Result<Vec<std::path::PathBuf>> {
    let requested = std::path::absolute(path)?;
    let mut paths = vec![requested.clone()];
    match std::fs::canonicalize(&requested) {
        Ok(target) if target != requested => paths.push(target),
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    Ok(paths)
}

fn config_watcher(
    path: &std::path::Path,
    trust: ConfigTrust,
    pending: std::sync::Arc<std::sync::atomic::AtomicBool>,
    send: impl Fn() -> bool + Send + 'static,
) -> Result<notify::RecommendedWatcher, String> {
    use notify::Watcher as _;

    prepare_config_directory(path, trust).map_err(|error| error.to_string())?;
    let watched = config_watch_paths(path).map_err(|error| error.to_string())?;
    let mut directories = Vec::new();
    for path in &watched {
        if let Some(parent) = path.parent()
            && !directories.iter().any(|directory| directory == parent)
        {
            directories.push(parent.to_path_buf());
        }
    }
    let watcher = notify::recommended_watcher(move |result: notify::Result<notify::Event>| {
        if let Ok(event) = result
            && watched
                .iter()
                .any(|path| watcher_event_matches(&event, path))
        {
            queue_watcher_event(&pending, &send);
        }
    })
    .map_err(|error| error.to_string())?;
    registered_watcher(watcher, |watcher| {
        for directory in directories {
            watcher.watch(&directory, notify::RecursiveMode::NonRecursive)?;
        }
        Ok::<_, notify::Error>(())
    })
    .map_err(|error| error.to_string())
}

fn registered_watcher<W, E>(
    mut watcher: W,
    register: impl FnOnce(&mut W) -> Result<(), E>,
) -> Result<W, E> {
    register(&mut watcher)?;
    Ok(watcher)
}

fn prepare_config_directory(path: &std::path::Path, trust: ConfigTrust) -> std::io::Result<()> {
    if trust == ConfigTrust::ExplicitPath {
        return Ok(());
    }
    let directory = path.parent().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("config path has no parent: {}", path.display()),
        )
    })?;
    if let Err(error) = kettle_state::create_private_dirs(directory) {
        log::debug!(
            "config directory repair skipped for {}: {error}",
            directory.display()
        );
    }
    kettle_state::validate_trusted_directory(directory)
}

fn resolved_init_lua_script(
    explicit: Option<std::path::PathBuf>,
    default_config: Option<std::path::PathBuf>,
) -> Option<(std::path::PathBuf, ConfigTrust)> {
    explicit
        .map(|path| (path, ConfigTrust::ExplicitPath))
        .or_else(|| {
            default_config
                .and_then(|path| path.parent().map(|dir| dir.join("init.lua")))
                .filter(|path| path.exists())
                .map(|path| (path, ConfigTrust::VerifyDirectory))
        })
}

fn reloaded_font_size(configured_changed: bool, configured: f32, runtime: Option<f32>) -> f32 {
    if configured_changed {
        configured
    } else {
        runtime.unwrap_or(configured)
    }
}

/// Scan `text` for any compiled trigger match, returning the
/// first action that fires. Pure helper used by `App::run_triggers` and
/// the drift guard. Returns `None` when no trigger fires so the caller
/// can skip the urgency-attention call.
fn match_triggers(
    text: &str,
    triggers: &[(regex::Regex, kettle_config::TriggerAction)],
) -> Option<kettle_config::TriggerAction> {
    triggers.iter().find_map(|(re, action)| {
        let caps = re.captures(text)?;
        Some(match action {
            // The matched pattern's capture groups substitute into the
            // command's argv (`{0}` whole match, `{1}`… numbered groups),
            // like `cmd.format(*groups)` in Terminator's `run_cmd_on_match.py`.
            kettle_config::TriggerAction::RunCommand(argv) => {
                kettle_config::TriggerAction::RunCommand(substitute_trigger_groups(argv, &caps))
            }
            other => other.clone(),
        })
    })
}

/// Replace `{0}`/`{1}`… in each argv element with the trigger match's
/// capture groups. `{0}` is the whole match and a non-participating group
/// substitutes empty. An out-of-range reference like `{9}` with two groups
/// stays literal, so the config typo shows in the spawned command instead of
/// silently vanishing. Substitution is per element, so argv stays argv.
/// Matched output can inject an argument's value but never new arguments or
/// shell metacharacters (the spawn is `std::process::Command`, no shell).
/// Pure (unit-tested).
fn substitute_trigger_groups(argv: &[String], caps: &regex::Captures) -> Vec<String> {
    argv.iter()
        .map(|a| {
            if !a.contains('{') {
                return a.clone();
            }
            // Single left-to-right pass over the template. Substituted text is
            // emitted verbatim and never re-scanned, so a capture whose matched
            // text contains `{2}` cannot expand again with attacker-controlled
            // content.
            let bytes = a.as_bytes();
            let mut out = String::with_capacity(a.len());
            let mut i = 0;
            while i < bytes.len() {
                if bytes[i] == b'{' {
                    let mut j = i + 1;
                    while j < bytes.len() && bytes[j].is_ascii_digit() {
                        j += 1;
                    }
                    if j > i + 1
                        && j < bytes.len()
                        && bytes[j] == b'}'
                        && let Ok(idx) = a[i + 1..j].parse::<usize>()
                        && idx < caps.len()
                    {
                        out.push_str(caps.get(idx).map(|m| m.as_str()).unwrap_or(""));
                        i = j + 1;
                        continue;
                    }
                    // Not a valid in-range placeholder: the `{` stays
                    // literal (an out-of-range `{9}` keeps the typo visible).
                    out.push('{');
                    i += 1;
                } else {
                    let ch = a[i..].chars().next().expect("in-bounds char");
                    out.push(ch);
                    i += ch.len_utf8();
                }
            }
            out
        })
        .collect()
}

/// One on-screen quick-select target: where its label sits and what it is.
#[derive(Clone)]
pub(crate) struct HintTarget {
    row: usize,
    col: usize,
    label: String,
    /// The pane the target was read from, whose directory a relative path
    /// resolves against, even if focus moved or that pane exited since.
    pane: u64,
    what: HintWhat,
}

/// What a quick-select target is.
#[derive(Clone)]
enum HintWhat {
    /// Text the detector found: a URL, a path, or something to copy.
    Text {
        kind: kettle_core::hints::Kind,
        text: String,
        /// The text meets a boundary on both sides (see `HintSpan::bounded`).
        bounded: bool,
    },
    /// An inline card on screen: picking it opens its item in the viewer.
    Card(kettle_core::InlineNonce),
}

/// A card's place in a pane for quick select: its first and last visible
/// cells, as (row, column), and its nonce.
type CardCells = ((usize, usize), (usize, usize), kettle_core::InlineNonce);

/// The quick-select targets in a pane, in reading order: each text span the
/// detector found outside every card, and each card at its first visible
/// cell. A card's rows hold its marks, not text a user would pick.
fn hint_targets_with_cards(
    spans: Vec<kettle_core::hints::HintSpan>,
    cards: &[CardCells],
) -> Vec<(usize, usize, HintWhat)> {
    let inside_card = |row: usize, col: usize| {
        cards.iter().any(|((top, left), (bottom, right), _)| {
            (*top..=*bottom).contains(&row) && (*left..=*right).contains(&col)
        })
    };
    let mut found: Vec<(usize, usize, HintWhat)> = spans
        .into_iter()
        .filter(|span| !inside_card(span.row, span.start))
        .map(|span| {
            (
                span.row,
                span.start,
                HintWhat::Text {
                    kind: span.kind,
                    text: span.text,
                    bounded: span.bounded,
                },
            )
        })
        .collect();
    found.extend(
        cards
            .iter()
            .map(|((row, col), _, nonce)| (*row, *col, HintWhat::Card(*nonce))),
    );
    found.sort_by_key(|(row, col, _)| (*row, *col));
    found
}

/// What picking a quick-select hint does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HintAction {
    OpenUrl,
    OpenPath,
    Copy,
}

/// A URL opens and anything else is copied; a label typed with Shift
/// (`alternate`) copies a URL and opens a path. A path that may not name a
/// whole local file (`unsafe_path`: from a pane that may be remote, or cut
/// short by a character the pattern does not take) is copied, never opened.
fn hint_action(kind: kettle_core::hints::Kind, alternate: bool, unsafe_path: bool) -> HintAction {
    use kettle_core::hints::Kind;
    match (kind, alternate) {
        (Kind::Url, false) => HintAction::OpenUrl,
        (Kind::Path, true) if !unsafe_path => HintAction::OpenPath,
        _ => HintAction::Copy,
    }
}

/// Whether a file named `name` is one a lane can preview, by its extension:
/// the raster formats the media worker decodes, SVG, Mermaid, Markdown,
/// whose Mermaid diagrams are a gallery, PDF, and the common video
/// containers, shown as a poster (`.ts` is left out: it names TypeScript far
/// more often).
/// The worker still decides by the bytes; this only picks what to offer.
fn previewable_name(name: &str) -> bool {
    let extension = name
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase());
    matches!(
        extension.as_deref(),
        Some(
            "png"
                | "jpg"
                | "jpeg"
                | "webp"
                | "bmp"
                | "gif"
                | "tif"
                | "tiff"
                | "heic"
                | "heif"
                | "avif"
                | "svg"
                | "mmd"
                | "mermaid"
                | "md"
                | "markdown"
                | "pdf"
                | "mp4"
                | "m4v"
                | "mov"
                | "webm"
                | "mkv"
                | "avi"
                | "mpg"
                | "mpeg"
                | "m2ts"
                | "mts"
                | "ogv"
                | "wmv"
                | "flv"
        )
    )
}

/// What quick select for previews offers: text naming a local file a lane
/// can preview, a path or a `file://` link, as `preview_link` labels it. A
/// path off Windows only, where `hint_path_url` resolves one.
fn hint_previews(what: &HintWhat) -> bool {
    use kettle_core::hints::Kind;
    match what {
        HintWhat::Text {
            kind: Kind::Path,
            text,
            bounded: true,
        } => !cfg!(windows) && previewable_name(text),
        HintWhat::Text {
            kind: Kind::Url,
            text,
            ..
        } => kettle_core::links::local_file_path(text)
            .is_some_and(|path| previewable_name(&path.to_string_lossy())),
        _ => false,
    }
}

/// What the clipboard names for a lane to show.
#[derive(Debug, PartialEq, Eq)]
enum CopiedPreview {
    /// A file a file manager copied: on this computer, as no pane's output
    /// can put a file on the clipboard.
    File(std::path::PathBuf),
    /// A copied path or `file://` link, as a `file://` URL. It names a file
    /// wherever the pane it was copied from runs, so the pane's gate decides.
    Link(String),
}

/// The lane control a lane action works as, for the focused pane's lane.
fn lane_action_hit(action: &Action) -> Option<kettle_render::MediaLaneHit> {
    use kettle_render::MediaLaneHit as Hit;
    match action {
        Action::PreviewNext => Some(Hit::Next),
        Action::PreviewPrevious => Some(Hit::Previous),
        Action::PreviewNextDiagram => Some(Hit::NextPage),
        Action::PreviewPreviousDiagram => Some(Hit::PreviousPage),
        Action::ClosePreview => Some(Hit::Close),
        Action::PreviewSource => Some(Hit::Mode),
        Action::PreviewCanvas => Some(Hit::Canvas),
        Action::PreviewCopy => Some(Hit::Copy),
        Action::PreviewReload => Some(Hit::Reload),
        Action::VideoPlay => Some(Hit::Play),
        Action::VideoMute => Some(Hit::Sound),
        Action::PreviewZoomIn => Some(Hit::ZoomIn),
        Action::PreviewZoomOut => Some(Hit::ZoomOut),
        Action::PreviewFit => Some(Hit::Fit),
        _ => None,
    }
}

/// The modifiers physically held. On macOS, Option counts whether or not it
/// composes text (`macos-option-as-alt`), so an Option chord is a chord.
fn held_modifiers(ws: &WindowState) -> winit::keyboard::ModifiersState {
    #[cfg(target_os = "macos")]
    {
        ws.macos_raw_mods
    }
    #[cfg(not(target_os = "macos"))]
    {
        ws.mods
    }
}

/// Why a control client's keys or text are refused while a preview lane in
/// the pane's window holds the keyboard.
const PREVIEW_HOLDS_KEYBOARD: &str =
    "a preview lane in this pane's window holds the keyboard; nothing is typed until Esc";

/// Longest copied text read as a path, in bytes.
const MAX_COPIED_PATH: usize = 4096;

/// The file the clipboard names for a lane to show: the first of `files`, as
/// a file manager copies them, that a lane can show, or else the one path or
/// `file://` link `text` holds. A path is resolved from the pane's `cwd` as
/// quick select resolves one, after one pair of quotes around it, or a
/// shell's backslash escapes, are undone. `None` when neither names one.
fn copied_preview(
    files: &[std::path::PathBuf],
    text: Option<&str>,
    cwd: Option<&str>,
    home: Option<&str>,
) -> Option<CopiedPreview> {
    let shows = |path: &std::path::Path| previewable_name(&path.to_string_lossy());
    if !files.is_empty() {
        // Through a file URL and back, so a listed file meets the checks a
        // link does: absolute, on this computer (no `\\host\share`), no `..`.
        // It must come back as itself: a Linux file list can name `C:/x`,
        // which is relative there. The CR a CRLF list leaves on a line is no
        // part of the name.
        return files.iter().find_map(|file| {
            let name = file.to_str()?;
            let file = std::path::Path::new(name.strip_suffix('\r').unwrap_or(name));
            kettle_core::links::file_url_for_path(file)
                .and_then(|url| kettle_core::links::local_file_path(&url))
                .filter(|path| path == file && shows(path))
                .map(CopiedPreview::File)
        });
    }
    // Two lines are no one path: a line break decodes to a control
    // character, which `local_file_path` refuses below. A path longer than
    // any URL Kettle accepts is dropped before it is unescaped and encoded.
    let text = text?.trim();
    if text.len() > MAX_COPIED_PATH {
        return None;
    }
    let url = if text
        .get(..7)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file://"))
    {
        text.to_string()
    } else if text.contains("://") {
        return None;
    } else {
        copied_path_url(&unquote_copied_path(text), cwd, home)?
    };
    kettle_core::links::local_file_path(&url)
        .is_some_and(|path| shows(&path))
        .then_some(CopiedPreview::Link(url))
}

/// A copied path without one pair of matching quotes around it or, off
/// Windows, without a shell's backslash escapes (`my\ plot.png`).
fn unquote_copied_path(text: &str) -> String {
    for quote in ['"', '\''] {
        if let Some(inner) = text
            .strip_prefix(quote)
            .and_then(|rest| rest.strip_suffix(quote))
        {
            return inner.to_string();
        }
    }
    if cfg!(windows) {
        return text.to_string();
    }
    let mut unescaped = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        unescaped.push(match c {
            '\\' => chars.next().unwrap_or(c),
            _ => c,
        });
    }
    unescaped
}

/// The `file://` URL a copied path names: off Windows as quick select
/// resolves a path (`hint_path_url`); on Windows only a drive path, never a
/// share or a relative path.
fn copied_path_url(text: &str, cwd: Option<&str>, home: Option<&str>) -> Option<String> {
    if cfg!(windows) {
        let bytes = text.as_bytes();
        let drive = bytes.len() > 2
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/');
        return drive
            .then(|| kettle_core::links::file_url_for_path(std::path::Path::new(text)))
            .flatten();
    }
    hint_path_url(text, cwd, home)
}

/// Most previews the user asked for that wait for their window to open them.
const MAX_PREVIEWS_READY: usize = 8;

/// Tell the user why a preview they asked for did not open; nothing for a
/// request that answers a control client (`None`).
fn notify_preview_failure(tr: kettle_i18n::Translator, failure: Option<kettle_media::FailureCode>) {
    let Some(failure) = failure else {
        return;
    };
    fire_notify(
        tr.text(kettle_i18n::Text::NotifyTitlePreviewFailed),
        tr.text(preview_failure_body(failure)),
    );
}

/// What a failed preview's notice says, by its failure.
fn preview_failure_body(failure: kettle_media::FailureCode) -> kettle_i18n::Text {
    use kettle_i18n::Text as T;
    use kettle_media::FailureCode as F;
    match failure {
        F::FileNotFound => T::NotifyBodyPreviewMissing,
        F::FilePermission | F::FileNotRegular | F::Changed => T::NotifyBodyPreviewUnreadable,
        F::TooLarge | F::FileTooLarge => T::NotifyBodyPreviewTooLarge,
        // Only a video's decoders answer these: which one a user can install.
        F::UnsupportedContainer | F::CodecUnavailable | F::BackendUnavailable => {
            T::NotifyBodyPreviewNeedsDecoder
        }
        // Installing anything would not help: the system has no sandbox.
        F::SandboxUnavailable => T::NotifyBodyPreviewNeedsSandbox,
        F::UnsupportedMedia | F::RenderParse | F::IndexOutOfRange => {
            T::NotifyBodyPreviewUnsupported
        }
        F::WorkerUnavailable | F::RestartRequired | F::UnsupportedPlatform => {
            T::NotifyBodyPreviewUnavailable
        }
        _ => T::NotifyBodyPreviewFailed,
    }
}

/// Where a link handed to `App::open_url` came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkSource {
    /// Kettle's own link (the release page).
    Kettle,
    /// A pane's output, with that pane's origin (`None`: it has gone).
    Pane(Option<PathOrigin>),
}

/// What opening a link from a pane's output does (see `App::open_pane_link`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LinkGate {
    Open,
    /// A local file link from a remote pane, or one that has gone.
    Refuse,
}

/// Whether a link from a pane with `origin` (`None`: the pane has gone) may
/// open. Only a `file://` link names a local file; web and mail links open
/// wherever they were printed. A multiplexer's server runs on this computer,
/// so its panes' file links open as local ones do; a file an ssh inside it
/// named is usually missing here, which the opener reports.
fn link_gate(uri: &str, origin: Option<PathOrigin>) -> LinkGate {
    let file_link = uri
        .get(..7)
        .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file://"));
    match (file_link, origin) {
        (false, _) | (true, Some(PathOrigin::Local)) => LinkGate::Open,
        (true, Some(PathOrigin::Remote) | None) => LinkGate::Refuse,
    }
}

/// The name a prompt or notification shows for a file link: its file name,
/// or the link itself when it names none. The text is untrusted, so
/// characters that reorder or hide text (bidirectional controls, zero-width
/// and other format characters) show as `\u{FFFD}`, and a name wider than
/// `max_cols` columns keeps its start and end around an ellipsis, so the
/// warning beside it stays readable.
fn link_display_name(uri: &str, max_cols: usize) -> String {
    use unicode_width::UnicodeWidthChar as _;
    let name = kettle_core::links::local_file_path(uri)
        .and_then(|path| {
            path.file_name()
                .map(|name| name.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| uri.to_string());
    let shown = name
        .chars()
        .map(|c| {
            let format = matches!(
                c,
                '\u{00AD}'
                    | '\u{061C}'
                    | '\u{180E}'
                    | '\u{200B}'..='\u{200F}'
                    | '\u{2028}'..='\u{202E}'
                    | '\u{2060}'..='\u{206F}'
                    | '\u{FEFF}'
                    | '\u{FFF9}'..='\u{FFFB}'
            );
            if c.is_control() || format {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect::<Vec<_>>();
    let width = |chars: &[char]| chars.iter().map(|c| c.width().unwrap_or(0)).sum::<usize>();
    if width(&shown) <= max_cols {
        return shown.into_iter().collect();
    }
    // Half the budget, less the ellipsis, from each end.
    let side = max_cols.saturating_sub(1) / 2;
    let mut head = 0;
    let mut used = 0;
    while let Some(c) = shown.get(head) {
        let w = c.width().unwrap_or(0);
        if used + w > side {
            break;
        }
        used += w;
        head += 1;
    }
    let mut tail = shown.len();
    used = 0;
    while tail > head {
        let w = shown[tail - 1].width().unwrap_or(0);
        if used + w > side {
            break;
        }
        used += w;
        tail -= 1;
    }
    shown[..head]
        .iter()
        .chain(['…'].iter())
        .chain(&shown[tail..])
        .collect()
}

/// The pane `ws.links` were scanned from: the focused pane at the last scan.
fn links_pane(ws: &WindowState) -> Option<u64> {
    ws.links_scan_key.as_ref().and_then(|key| key.0.1)
}

/// Where the files a pane's output names live, as far as Kettle can tell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PathOrigin {
    /// This computer, including behind tmux, screen or zellij, whose server
    /// runs here.
    Local,
    /// Another machine or a container: a detected remote session, or a
    /// remote shell or container client in the pane.
    Remote,
}

/// Where a pane's printed paths live: remote when it runs a detected remote
/// session or a remote or container client, and local otherwise.
///
/// What runs now decides: the foreground program, shell scripts included
/// (`sh -c 'tmux attach'`). The command the pane was launched with counts
/// only when it is itself a client (`ssh host`, `env TERM=x ssh host`): a
/// launch script that ran ssh and then `exec bash` must not mark the pane
/// remote for good. Output a pane printed while it was remote stays on
/// screen after it returns local; links are gated by what runs when they are
/// opened.
fn pane_path_origin(pane: &crate::mux::Pane) -> PathOrigin {
    path_origin(
        pane.remote_context.is_some() || argv_is_nonlocal_client(&pane.argv),
        &pane.argv,
        pane.foreground_process
            .as_ref()
            .map(|process| process.argv.as_slice()),
    )
}

/// [`pane_path_origin`] from its parts: whether a remote session is known,
/// the launch command and the foreground program's command line.
fn path_origin(remote: bool, launch: &[String], foreground: Option<&[String]>) -> PathOrigin {
    let launch_client = argv_client_with(launch, 0, false);
    let foreground_client = foreground.and_then(|argv| argv_client(argv, 0));
    match (
        remote,
        any_client([launch_client, foreground_client].into_iter()),
    ) {
        (true, _) | (false, Some(Client::Remote)) => PathOrigin::Remote,
        (false, None) => PathOrigin::Local,
    }
}

/// What kind of client a command line runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Client {
    Remote,
}

/// The remote or container client `argv` runs, looking through a shell's
/// `-c` script, every command in it (`sh -c 'cd ~ && "ssh" host'`), and
/// through wrappers that run another command (`env TERM=xterm ssh host`,
/// `sudo -u me ssh host`, `nice -n 5 mosh host`). Nesting deeper than four
/// levels counts as remote: it fails closed rather than reading as local.
fn argv_client(argv: &[String], depth: usize) -> Option<Client> {
    argv_client_with(argv, depth, true)
}

/// [`argv_client`], reading shell `-c` scripts only when `scripts` is set: a
/// pane's launch command counts only when it is itself a client, wrapped or
/// not, since a launch script may long since have moved on.
fn argv_client_with(argv: &[String], depth: usize, scripts: bool) -> Option<Client> {
    if depth > 4 {
        return Some(Client::Remote);
    }
    let argv0 = argv.first()?;
    let name = argv0
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(argv0)
        .to_ascii_lowercase();
    let name = name.strip_suffix(".exe").unwrap_or(&name);
    let rest = &argv[1..];
    match name {
        "ssh" | "autossh" | "mosh" | "mosh-client" | "et" | "telnet" | "docker" | "podman"
        | "kubectl" | "lxc-attach" => Some(Client::Remote),
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "mksh" | "fish" if !scripts => None,
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "mksh" | "fish" => {
            // The argument after a flag cluster holding `c` (`-c`, `-lc`).
            let flag = rest.iter().position(|arg| {
                arg.len() > 1
                    && arg.starts_with('-')
                    && !arg.starts_with("--")
                    && arg[1..].contains('c')
            })?;
            // A script past the parser's bounds fails closed.
            let Some(commands) = shell_commands(rest.get(flag + 1)?) else {
                return Some(Client::Remote);
            };
            any_client(commands.iter().map(|words| {
                let words = command_words(words);
                // A command the parser cannot name (`$cmd`, `` `cmd` ``,
                // `eval …`) fails closed.
                match words.first() {
                    Some(first) if first == "eval" || first.contains(['$', '`']) => {
                        Some(Client::Remote)
                    }
                    _ => argv_client(words, depth + 1),
                }
            }))
        }
        "env" | "nohup" | "nice" | "caffeinate" | "sudo" | "doas" | "time" | "sshpass" => {
            // Options that take a separate value, per wrapper.
            let takes_value: &[&str] = match name {
                "env" => &["-u", "-C", "--unset", "--chdir"],
                "nice" => &["-n", "--adjustment"],
                "caffeinate" => &["-t", "-w"],
                "sudo" => &[
                    "-u",
                    "-g",
                    "-h",
                    "-p",
                    "-C",
                    "-D",
                    "-r",
                    "-t",
                    "-U",
                    "-T",
                    "--user",
                    "--group",
                    "--host",
                    "--prompt",
                    "--close-from",
                    "--chdir",
                    "--role",
                    "--type",
                    "--other-user",
                    "--command-timeout",
                ],
                "doas" => &["-u", "-C", "-a"],
                "time" => &["-o", "-f", "--output", "--format"],
                "sshpass" => &["-p", "-f", "-d", "-P"],
                _ => &[],
            };
            let mut index = 0;
            while let Some(arg) = rest.get(index) {
                if arg == "--" {
                    index += 1;
                    break;
                } else if arg.starts_with('-') {
                    index += if takes_value.contains(&arg.as_str()) {
                        2
                    } else {
                        1
                    };
                } else if name == "env" && arg.contains('=') {
                    index += 1;
                } else {
                    break;
                }
            }
            // An option this table does not know (`--preserve-env X`) may
            // have taken the next word as its value: then the command starts
            // one word later.
            let uncertain = index
                .checked_sub(1)
                .and_then(|before| rest.get(before))
                .is_some_and(|option| {
                    option.starts_with('-')
                        && option != "--"
                        && !option.contains('=')
                        && !takes_value.contains(&option.as_str())
                });
            argv_client_with(rest.get(index..)?, depth + 1, scripts).or_else(|| {
                uncertain
                    .then(|| argv_client_with(rest.get(index + 1..)?, depth + 1, scripts))
                    .flatten()
            })
        }
        _ => None,
    }
}

/// The client any of `clients` runs.
fn any_client(mut clients: impl Iterator<Item = Option<Client>>) -> Option<Client> {
    clients.find_map(|client| client)
}

/// A command's words past leading redirections and assignments, the shell
/// keywords before a
/// command (`if`, `then`, `do`, `!`, `{`…), and the precommand words the shell
/// runs it through (`exec`, `command`, `builtin`, `nohup`, `noglob`,
/// `nocorrect`) with their options (`command -p`, `exec -a NAME`).
fn command_words(words: &[String]) -> &[String] {
    let mut index = 0;
    while let Some(word) = words.get(index) {
        // A redirection (`2>/dev/null`, `>out`, `&>log`), and its target when
        // it stands apart (`2> /dev/null`).
        let redirection = word.trim_start_matches(|c: char| c.is_ascii_digit() || c == '&');
        if redirection.starts_with(['<', '>']) {
            let operator_only = redirection
                .trim_start_matches(['<', '>', '&', '|'])
                .is_empty();
            index += if operator_only { 2 } else { 1 };
        } else if word.contains('=') && !word.starts_with('=') && !word.starts_with('-') {
            index += 1;
        } else if matches!(
            word.as_str(),
            "if" | "then"
                | "else"
                | "elif"
                | "do"
                | "while"
                | "until"
                | "!"
                | "{"
                | "not"
                | "and"
                | "or"
                | "begin"
        ) {
            // Shell keywords that come before a command, fish's included.
            index += 1;
        } else if matches!(
            word.as_str(),
            "exec" | "command" | "builtin" | "nohup" | "noglob" | "nocorrect"
        ) {
            let precommand = word.as_str();
            index += 1;
            while let Some(option) = words.get(index).filter(|option| option.starts_with('-')) {
                // `command -v ssh` names a program without running it.
                if precommand == "command" && option.contains(['v', 'V']) {
                    return &[];
                }
                index += if precommand == "exec" && option == "-a" {
                    2
                } else {
                    1
                };
                if option == "--" {
                    break;
                }
            }
        } else {
            break;
        }
    }
    words.get(index..).unwrap_or_default()
}

/// The simple commands in a shell script, as words with their quotes
/// removed: `;`, `&`, `|`, newlines and parentheses separate commands, and
/// single quotes, double quotes and backslashes quote. Command substitutions
/// (`$(…)`, `` `…` ``) are commands too, nested at most four deep; in command
/// position one reads as `$SUBST`, which a caller cannot name. `None` for a
/// script past the bounds (4 KiB, 16 commands, the nesting) or with a
/// substitution left open, which a caller must not read as holding no
/// client.
fn shell_commands(script: &str) -> Option<Vec<Vec<String>>> {
    shell_commands_at(script, 0)
}

fn shell_commands_at(script: &str, depth: usize) -> Option<Vec<Vec<String>>> {
    if script.len() > 4096 || depth > 4 {
        return None;
    }
    let (outer, substitutions) = split_substitutions(script)?;
    let mut commands = split_commands(&outer)?;
    for substitution in substitutions {
        commands.extend(shell_commands_at(&substitution, depth + 1)?);
        if commands.len() > 16 {
            return None;
        }
    }
    Some(commands)
}

/// `script` with each command substitution outside single quotes replaced by
/// `$SUBST`, and the substitutions' own text. `None` for one left open.
fn split_substitutions(script: &str) -> Option<(String, Vec<String>)> {
    let mut outer = String::with_capacity(script.len());
    let mut substitutions = Vec::new();
    let mut chars = script.chars().peekable();
    let (mut single, mut double) = (false, false);
    while let Some(c) = chars.next() {
        match c {
            '\\' if !single => {
                outer.push(c);
                outer.extend(chars.next());
            }
            '\'' if !double => {
                single = !single;
                outer.push(c);
            }
            '"' if !single => {
                double = !double;
                outer.push(c);
            }
            '$' if !single && chars.peek() == Some(&'(') => {
                chars.next();
                let mut depth = 1usize;
                let mut body = String::new();
                // Parentheses inside quotes do not count (`$(printf ')')`).
                let (mut inner_single, mut inner_double) = (false, false);
                loop {
                    let c = chars.next()?;
                    match c {
                        '\\' if !inner_single => {
                            body.push(c);
                            body.extend(chars.next());
                            continue;
                        }
                        '\'' if !inner_double => inner_single = !inner_single,
                        '"' if !inner_single => inner_double = !inner_double,
                        '(' if !inner_single && !inner_double => depth += 1,
                        ')' if !inner_single && !inner_double => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 {
                        break;
                    }
                    body.push(c);
                }
                substitutions.push(body);
                outer.push_str("$SUBST");
            }
            '`' if !single => {
                let mut body = String::new();
                loop {
                    match chars.next()? {
                        '`' => break,
                        '\\' => body.extend(chars.next()),
                        c => body.push(c),
                    }
                }
                substitutions.push(body);
                outer.push_str("$SUBST");
            }
            c => outer.push(c),
        }
    }
    Some((outer, substitutions))
}

/// The simple commands in a script with no substitutions left (see
/// [`shell_commands`]).
fn split_commands(script: &str) -> Option<Vec<Vec<String>>> {
    let mut commands = vec![Vec::new()];
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = script.chars();
    let end_word = |word: &mut String, in_word: &mut bool, commands: &mut Vec<Vec<String>>| {
        if *in_word {
            commands
                .last_mut()
                .expect("a command")
                .push(std::mem::take(word));
            *in_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                word.extend(chars.by_ref().take_while(|&c| c != '\''));
            }
            '"' => {
                in_word = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        // A backslash-newline continues the line.
                        '\\' => match chars.next() {
                            Some('\n') | None => {}
                            Some(c) => word.push(c),
                        },
                        c => word.push(c),
                    }
                }
            }
            '\\' => match chars.next() {
                // A backslash-newline continues the word.
                Some('\n') | None => {}
                Some(c) => {
                    in_word = true;
                    word.push(c);
                }
            },
            // A redirection starts a word of its own (`tmux</dev/tty`),
            // keeping a file descriptor before it (`2>`).
            '<' | '>' if in_word && !word.chars().all(|c| c.is_ascii_digit() || c == '&') => {
                end_word(&mut word, &mut in_word, &mut commands);
                in_word = true;
                word.push(c);
            }
            // `2>&1` and `&>file` are redirections, not command separators.
            '&' if word.ends_with(['>', '<']) || chars.clone().next() == Some('>') => {
                in_word = true;
                word.push(c);
            }
            ';' | '&' | '|' | '\n' | '(' | ')' => {
                end_word(&mut word, &mut in_word, &mut commands);
                if commands.last().is_some_and(|command| !command.is_empty()) {
                    if commands.len() == 16 {
                        return None;
                    }
                    commands.push(Vec::new());
                }
            }
            c if c.is_whitespace() => end_word(&mut word, &mut in_word, &mut commands),
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    end_word(&mut word, &mut in_word, &mut commands);
    commands.retain(|command| !command.is_empty());
    Some(commands)
}

/// The local `file://` URL a path hint names: `~/` resolved from `home`, a
/// relative path from the pane's `cwd`, `.` segments dropped. This touches no
/// filesystem: the path and an OSC 7 `cwd` are untrusted terminal text, and a
/// stat would let them reach a network share or an automount (`/net/host`),
/// leaking the user's address and stalling the UI thread. The OS opener, run
/// detached, resolves symlinks. `None`, so the hint is copied instead, when
/// the base is unknown or not local, or the path climbs with `..`, whose
/// meaning after a symlink only the filesystem knows; always on Windows,
/// where a link to a `\\host\share` would contact that host.
fn hint_path_url(text: &str, cwd: Option<&str>, home: Option<&str>) -> Option<String> {
    use std::path::{Component, Path, PathBuf};
    if cfg!(windows) {
        return None;
    }
    let local = |base: &str| cwd_is_local(base).then(|| Path::new(base).to_path_buf());
    let joined = if let Some(rest) = text.strip_prefix("~/") {
        local(home?)?.join(rest)
    } else if text.starts_with('/') {
        PathBuf::from(text)
    } else if text.as_bytes().get(1) == Some(&b':') {
        // A drive-letter path names nothing local off Windows.
        return None;
    } else {
        local(cwd?)?.join(text)
    };
    let mut resolved = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => return None,
            other => resolved.push(other),
        }
    }
    let text = resolved.to_str()?;
    if !cwd_is_local(text) || !resolved.is_absolute() {
        return None;
    }
    // Percent-encode everything but the characters a file URL's path keeps.
    let mut url = String::from("file://");
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~:".contains(&byte) {
            url.push(char::from(byte));
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    Some(url)
}

/// What a context-menu click dispatches, such as a kettle Action (built-in
/// items) or a Lua callback index (kettle.add_menu_item entries).
/// The rows a right-click on the link `url` from pane `pane`'s output leads
/// the menu with: open it, preview it in Kettle when it is a local file a lane
/// can show from a pane still known, copy its address, then a separator.
fn link_menu_rows(
    url: &str,
    pane: Option<u64>,
    tr: kettle_i18n::Translator,
) -> Vec<ContextMenuItem> {
    use kettle_i18n::Text as T;
    let row = |label: T, how: UrlHow| ContextMenuItem::UrlItem {
        label: tr.text(label),
        url: url.to_string(),
        how,
        pane,
    };
    let previewable = pane.is_some()
        && kettle_core::links::local_file_path(url)
            .is_some_and(|path| previewable_name(&path.to_string_lossy()));
    let mut rows = vec![row(T::MenuOpenLink, UrlHow::Open)];
    if previewable {
        rows.push(row(T::MenuPreviewInKettle, UrlHow::Preview));
    }
    rows.push(row(T::MenuCopyLink, UrlHow::Copy));
    rows.push(ContextMenuItem::Separator);
    rows
}

/// What a link row of the context menu does with its address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UrlHow {
    Open,
    Copy,
    /// Preview the local file it names in the pane's lane.
    Preview,
}

#[derive(Clone)]
enum ContextMenuClick {
    Action(Action),
    Preview {
        target: PreviewMenuTarget,
        action: PreviewMenuAction,
    },
    /// A media row: what it does, and to which shelf item.
    Media {
        target: MediaTarget,
        action: MediaMenuAction,
    },
    LuaMenuItem(usize),
    /// Terminator parity (`custom_commands.py`): a
    /// `menu-item = LABEL = CMD` config entry. Dispatch writes
    /// `CMD\n` to the focused pane's PTY.
    ConfigCommand(String),
    /// Reopen a detected session in a fresh local pane.
    ReconnectRemote(String),
    /// Profile picked from the right-click "Profile ▸" submenu.
    /// Dispatch sets `App::config_path` via the profile-path
    /// resolution helper and calls `reload_config`.
    SetProfile(String),
    MoveTabToWindow {
        window: u64,
        panes: Arc<[u64]>,
    },
    /// Drill into a `Submenu` row by index. The click handler pushes
    /// the current items onto `drill_stack` + replaces them
    /// with the submenu's items.
    DrillIntoSubmenu(usize),
    /// Open a new tab running an explicit argv (a shell picked from
    /// the new-tab `▾` dropdown). Dispatch calls `Mux::new_tab_with` with the
    /// focused tab's current working directory.
    NewTabWithArgv(Vec<String>),
    /// Terminator parity (terminal_popup_menu.py "Open link" /
    /// "Copy address"): a click on one of the URL-aware leading rows.
    /// `how` says what the row does with the address: open it through the
    /// `open_url` chain (Lua handler → custom_url_handler → system open,
    /// `is_safe_url`-guarded), copy it, or preview the local file it names
    /// in the pane's lane.
    Url {
        url: String,
        how: UrlHow,
        /// The pane whose output held the link; `None` for Kettle's own
        /// links (About).
        pane: Option<u64>,
    },
}

/// One entry in the right-click context menu. `Separator` rows render
/// as a thin divider in the menu and are skipped during keyboard nav
/// and click dispatch; `Item` rows carry the action to fire.
#[derive(Clone)]
enum ContextMenuItem {
    Item {
        label: &'static str,
        action: Action,
        enabled: bool,
    },
    /// Like `Item` but with an owned `String` label, so dynamic state
    /// markers (radio `● / ○`, check `✓ /  `) can be baked into the
    /// label at build time without leaking memory via `Box::leak`.
    /// Same dispatch surface as `Item` (typed `Action`).
    DynamicItem {
        label: String,
        action: Action,
        enabled: bool,
    },
    Separator,
    /// Terminator plugin parity:
    /// menu item supplied by a Lua plugin via `kettle.add_menu_item(
    /// label, callback)`. Dispatch invokes the registered Lua
    /// callback (looked up by `lua_idx` in the kettle_menu_items
    /// table) instead of an Action.
    LuaItem {
        label: String,
        lua_idx: usize,
    },
    /// Terminator parity (`custom_commands.py`): user-
    /// defined menu entry from a config-file `menu-item = LABEL =
    /// CMD` line. Dispatch writes `command + "\n"` to the focused
    /// PTY. Simpler than `LuaItem` — no callback, just literal
    /// text. Use this for "Run `clear`" / "Open `~/.bashrc` in
    /// `$EDITOR`" / etc.
    ConfigItem {
        label: String,
        command: String,
        /// Reconnect commands run in a fresh local pane.
        new_pane: bool,
    },
    /// A submenu row carries a nested item list; activation replaces the visible
    /// menu level.
    Submenu {
        label: String,
        // Consumed by the drill-in dispatch
        // (`ContextMenuClick::DrillIntoSubmenu`), which pushes the
        // parent items onto `drill_stack` and replaces them with
        // these.
        items: Vec<ContextMenuItem>,
    },
    WindowChoice {
        label: String,
        window: u64,
        panes: Arc<[u64]>,
    },
    /// A profile-choice leaf row used inside a
    /// `Submenu { label: "Profile", … }`. Clicking dispatches
    /// `ContextMenuClick::SetProfile(profile)` which switches the
    /// active profile (`App::config_path` + reload_config).
    ProfileChoice {
        label: String,
        profile: String,
    },
    /// A shell-choice leaf in the new-tab `▾` dropdown. Clicking
    /// dispatches `ContextMenuClick::NewTabWithArgv(argv)` to open a tab
    /// running that shell.
    NewTabShell {
        label: String,
        argv: Vec<String>,
    },
    /// Terminator parity (terminal_popup_menu.py "Open link" /
    /// "Copy address"): URL-aware leading rows, present only when the
    /// right-click landed on a detected hyperlink. The URL is captured at
    /// menu-open time so a subsequent output scroll can't retarget the click.
    /// Clicking dispatches `ContextMenuClick::Url { url, how }`.
    UrlItem {
        label: &'static str,
        url: String,
        how: UrlHow,
        /// The pane whose output held the link, captured with it.
        pane: Option<u64>,
    },
    /// A row of an inline card's menu: open its item in Kettle, or a copy of
    /// it in the permitted image viewer. The card and its item, with the
    /// item's generation, are captured when the menu opens.
    Media {
        label: &'static str,
        target: MediaTarget,
        action: MediaMenuAction,
    },
    /// Actions bound to the preview that opened the menu.
    Preview {
        label: &'static str,
        target: PreviewMenuTarget,
        action: PreviewMenuAction,
    },
    /// A static, non-dispatchable information line
    /// (the About panel's version/update rows). Rendered like a disabled row
    /// (dimmed), survives `filter_disabled`, never highlighted, claims no
    /// mnemonic, and maps to no click.
    Info {
        label: String,
    },
}

/// The card a media menu row acts on, and the shelf item it showed when the
/// menu opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct MediaTarget {
    pane: u64,
    nonce: kettle_core::InlineNonce,
    item: u64,
    generation: u64,
}

/// What a media menu row does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MediaMenuAction {
    /// Open the item in Kettle's viewer.
    View,
    /// Open a copy of it in the permitted image viewer, or of a video in
    /// the permitted video player.
    OpenOutside(crate::media::Outside),
}

/// Replacements and mode changes invalidate this menu's actions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PreviewMenuTarget {
    pane: u64,
    item: u64,
    generation: u64,
    mode: kettle_render::MediaLaneMode,
}

impl PreviewMenuTarget {
    fn matches(
        self,
        item: &crate::media::ShelfItem,
        panel: &crate::window_state::PreviewPanel,
    ) -> bool {
        panel.item == self.item
            && item.id == self.item
            && item.generation == self.generation
            && lane_mode(item, panel) == self.mode
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PreviewMenuAction {
    Copy(kettle_render::MediaLaneMode),
    Control(kettle_render::MediaLaneHit),
    Expanded(bool),
    Play(kettle_render::MediaLanePlay),
    Sound(kettle_render::MediaLaneSound),
    OpenOutside(crate::media::Outside),
}

/// What the title-edit overlay edits (Terminator parity).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TitleEditScope {
    /// Edit the OS window title (winit Window::set_title).
    Window,
    /// Edit the tab active when the overlay opens (overrides what the
    /// tab-bar shows independent of any OSC 1/2 from a pane).
    Tab,
    /// Edit the pane focused when the overlay opens (shown in its per-pane
    /// titlebar; the OSC-1 equivalent).
    Pane,
    /// Edit the opening pane's broadcast-group name (Terminator parity).
    /// Writes to pane.group_name. Empty input clears the group.
    Group,
}

/// Group assignment scope for the title-edit overlay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GroupBulkScope {
    /// Default: just the pane focused when the overlay opens (existing
    /// `EditPaneGroup` / `CreateGroup` behavior).
    #[default]
    Single,
    /// Every pane in the tab active when the overlay opens gets the typed name.
    Tab,
    /// Every pane in every tab.
    Window,
}

#[derive(Debug, Clone)]
pub struct TitleEditState {
    pub scope: TitleEditScope,
    /// Stable identity captured when the edit opens. Pane ids are never reused;
    /// a tab is named by every pane it held so it survives index shifts and one
    /// of its panes exiting.
    pub target: TitleEditTarget,
    /// Current text the user has typed. Pre-filled with the existing
    /// title so the user can edit in place vs starting blank.
    pub input: String,
    /// When `scope == Group`, which panes Apply writes
    /// to. Single = opening pane only (existing behavior); Tab/Window
    /// = bulk-assign via `Action::GroupTab`/`GroupWindow`.
    pub bulk: GroupBulkScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TitleEditTarget {
    /// The owning `WindowState`. Window-title edits and window-bulk group edits
    /// deliberately keep live window membership: the state itself cannot move
    /// to another window, and `GroupWindow` still means every pane present in
    /// this window when the name is applied.
    Window,
    /// All pane ids held by the target tab when the edit opened. Resolving any
    /// surviving id finds the same tab after reordering or sibling exits.
    Tab(Vec<u64>),
    /// One pane, used by pane-title and single-pane group edits.
    Pane(u64),
}

fn begin_title_edit(
    mux: &Mux,
    scope: TitleEditScope,
    bulk: GroupBulkScope,
    window_title: &str,
) -> Option<TitleEditState> {
    let target = match (scope, bulk) {
        (TitleEditScope::Window, _) | (TitleEditScope::Group, GroupBulkScope::Window) => {
            TitleEditTarget::Window
        }
        (TitleEditScope::Tab, _) | (TitleEditScope::Group, GroupBulkScope::Tab) => {
            let panes = mux.tab_anchor_panes(mux.active);
            if panes.is_empty() {
                return None;
            }
            TitleEditTarget::Tab(panes)
        }
        (TitleEditScope::Pane, _) | (TitleEditScope::Group, GroupBulkScope::Single) => {
            TitleEditTarget::Pane(mux.active_focus()?)
        }
    };
    let input = match (scope, bulk, &target) {
        (TitleEditScope::Window, _, TitleEditTarget::Window) => window_title.to_string(),
        (TitleEditScope::Tab, _, TitleEditTarget::Tab(panes)) => {
            let idx = mux.tab_index_of_any_pane(panes)?;
            mux.tabs[idx].title_override.clone().unwrap_or_default()
        }
        (TitleEditScope::Pane, _, TitleEditTarget::Pane(id)) => mux.panes.get(id)?.title.clone(),
        (TitleEditScope::Group, GroupBulkScope::Single, TitleEditTarget::Pane(id)) => {
            mux.panes.get(id)?.group_name.clone().unwrap_or_default()
        }
        (TitleEditScope::Group, GroupBulkScope::Tab, TitleEditTarget::Tab(_))
        | (TitleEditScope::Group, GroupBulkScope::Window, TitleEditTarget::Window) => String::new(),
        _ => return None,
    };
    Some(TitleEditState {
        scope,
        target,
        input,
        bulk,
    })
}

fn title_edit_target_is_live(mux: &Mux, state: &TitleEditState) -> bool {
    match (state.scope, state.bulk, &state.target) {
        (TitleEditScope::Window, _, TitleEditTarget::Window)
        | (TitleEditScope::Group, GroupBulkScope::Window, TitleEditTarget::Window) => true,
        (TitleEditScope::Tab, _, TitleEditTarget::Tab(panes))
        | (TitleEditScope::Group, GroupBulkScope::Tab, TitleEditTarget::Tab(panes)) => {
            mux.tab_index_of_any_pane(panes).is_some()
        }
        (TitleEditScope::Pane, _, TitleEditTarget::Pane(id))
        | (TitleEditScope::Group, GroupBulkScope::Single, TitleEditTarget::Pane(id)) => {
            mux.panes.contains_key(id)
        }
        _ => false,
    }
}

fn clear_stale_title_edit(mux: &Mux, editing: &mut Option<TitleEditState>) -> bool {
    if editing
        .as_ref()
        .is_some_and(|state| !title_edit_target_is_live(mux, state))
    {
        *editing = None;
        true
    } else {
        false
    }
}

fn apply_mux_title_edit(mux: &mut Mux, state: &TitleEditState) -> bool {
    let value = &state.input;
    match (state.scope, state.bulk, &state.target) {
        (TitleEditScope::Window, _, _) => false,
        (TitleEditScope::Tab, _, TitleEditTarget::Tab(panes)) => {
            let Some(idx) = mux.tab_index_of_any_pane(panes) else {
                return false;
            };
            let tab = &mut mux.tabs[idx];
            tab.title_override = if value.is_empty() {
                None
            } else {
                Some(value.clone())
            };
            true
        }
        (TitleEditScope::Pane, _, TitleEditTarget::Pane(id)) => {
            let Some(pane) = mux.panes.get_mut(id) else {
                return false;
            };
            // Empty restores shell-driven titles; anything else pins the name
            // against the next OSC 0/2.
            if value.trim().is_empty() {
                pane.title = "kettle".into();
                pane.title_is_placeholder = true;
                pane.title_origin = PaneTitleOrigin::Placeholder;
            } else {
                pane.title = value.clone();
                pane.title_is_placeholder = false;
                pane.title_origin = PaneTitleOrigin::Manual;
            }
            true
        }
        (TitleEditScope::Group, bulk, target) => {
            let next = if value.is_empty() {
                None
            } else {
                Some(value.clone())
            };
            match (bulk, target) {
                (GroupBulkScope::Single, TitleEditTarget::Pane(id)) => {
                    let Some(pane) = mux.panes.get_mut(id) else {
                        return false;
                    };
                    pane.group_name = next;
                    true
                }
                (GroupBulkScope::Tab, TitleEditTarget::Tab(panes)) => {
                    let Some(idx) = mux.tab_index_of_any_pane(panes) else {
                        return false;
                    };
                    let ids = mux
                        .tabs
                        .get(idx)
                        .map(|tab| tab.root.leaf_ids())
                        .unwrap_or_default();
                    let applied = !ids.is_empty();
                    for id in ids {
                        if let Some(pane) = mux.panes.get_mut(&id) {
                            pane.group_name = next.clone();
                        }
                    }
                    applied
                }
                (GroupBulkScope::Window, TitleEditTarget::Window) => {
                    for pane in mux.panes.values_mut() {
                        pane.group_name = next.clone();
                    }
                    true
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// Whether a window close should also empty the mux before the session is
/// saved.
///
/// `close_window` means "this window is finished" and saves an empty session so
/// the next launch starts fresh. The titlebar ✕ / Alt+F4 means "put this away"
/// and leaves the session restorable. Both close the window and differ only in
/// what the next launch shows. That must not depend on whether the
/// `ask-before-closing` prompt appeared in between, so the confirm action
/// carries this choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropPanes {
    Yes,
    No,
}

/// Deferred actions for close confirmation and keybind conflicts.
#[allow(clippy::enum_variant_names)] // close-family prefix is intentional
#[derive(Debug, Clone)]
pub enum ConfirmAction {
    /// Close the entire window (every tab + every pane).
    ///
    /// `drop_panes` carries the requester's intent through the prompt, so
    /// confirming does what the gesture would have done unprompted.
    CloseWindow { drop_panes: DropPanes },
    /// Close whichever tab holds any of these panes.
    ///
    /// Every tab close routes through here, including the focused-tab one.
    /// A prompt can sit unanswered for as long as the user takes, and in that
    /// time a shell exiting renumbers the tabs — so "the tab at index N" and
    /// "the focused tab" both stop meaning what they meant when the prompt
    /// went up. Naming the tab by the panes it held is stable across that; an
    /// empty resolution means the tab is already gone.
    CloseTabHolding(Vec<u64>),
    /// Close this specific pane.
    ///
    /// Carries the id for the same reason: while the prompt is up, the
    /// focused pane can change — the target's own shell exiting promotes a
    /// sibling into focus — and confirming would then close the sibling, which
    /// may be a tmux or agent session the user never selected.
    ClosePane(u64),
    /// Finish a Settings keybind rebind that would otherwise silently steal
    /// a chord already bound to a different action. Once the user confirms
    /// the reassignment, applies via `App::apply_keybind_rebind`, the same
    /// call as the direct (no-conflict) rebind path.
    RebindKeybind {
        trig: Trigger,
        act: Action,
        action_name: &'static str,
    },
}

/// Which buttons a confirm modal shows. v1 is just
/// the two-button [Cancel] / [Confirm] shape; future additions can
/// add a third "Apply to all" or similar without rippling.
#[derive(Debug, Clone)]
pub enum ConfirmButton {
    /// Dismiss the modal without action. Always the safe default.
    Cancel,
    /// Dispatch the dialog's action using destructive or ordinary styling.
    Confirm { label: String, destructive: bool },
}

/// Live state for an open confirm dialog. `focus_idx`
/// points into `buttons` and the renderer / keyboard nav follow it.
#[derive(Debug, Clone)]
pub struct ConfirmDialogState {
    pub heading: String,
    pub body: String,
    pub buttons: Vec<ConfirmButton>,
    pub focus_idx: usize,
    pub on_confirm: ConfirmAction,
    pub scroll: usize,
    pub armed_press: Option<(usize, (f32, f32, f32, f32))>,
    pub space_pressed: Option<usize>,
}

/// UI-side context-menu state (Terminator / GNOME / iTerm2 parity).
/// Anchor is the post-clamp panel top-left; rows mirror the renderer's
/// `ContextMenu` slice but carry the live `Action` for dispatch.
pub(crate) struct ContextMenuState {
    anchor: (f32, f32),
    /// DPI scale at which `anchor` was captured. Anchors are physical pixels;
    /// rescale them across monitor transitions before clamping so the menu
    /// keeps the same logical relationship to the pointer.
    anchor_scale: f64,
    items: Vec<ContextMenuItem>,
    /// Selected dispatchable row; never a separator or disabled row.
    highlight: usize,
    /// Parent levels, including the selected submenu and scroll position.
    drill_stack: Vec<ContextMenuLevel>,
    /// First visible item in a height-clamped menu.
    scroll_offset: usize,
    /// Pending keyboard prefix, cleared after 750ms of inactivity.
    typeahead_buf: String,
    /// Deadline after which `typeahead_buf` is cleared
    /// on the next key. `None` when the buffer is empty.
    typeahead_until: Option<std::time::Instant>,
}

struct ContextMenuLevel {
    items: Vec<ContextMenuItem>,
    highlight: usize,
    scroll_offset: usize,
}

/// Pure: which segment index the tab-bar cursor falls in,
/// using the same rendered segment rects that hit-testing and painting use.
/// Used by the drag-to-reorder handler — the user grabs a tab,
/// drags, and the bar reorders to keep the dragged segment under the cursor.
/// Clamped to the first/last segment on screen (scrolled-out tabs are never
/// targets) so a cursor that overshoots either edge still produces a valid
/// target. Returns 0 for an empty bar.
fn tab_drag_target_index(
    cursor_x: f32,
    cursor_y: f32,
    segments: &[kettle_render::TabSeg],
) -> usize {
    let Some(first) = segments.first() else {
        return 0;
    };
    // Which way the strip runs is a property of what was drawn: a vertical bar
    // stacks its segments down a shared column, a horizontal one lays them
    // across a shared row. Reading it back off the segments means the drag can
    // never disagree with the geometry the user is looking at.
    //
    // Testing `cursor_x` alone would break vertical bars, where every segment
    // shares one x span and the first one would always match.
    let vertical = segments.iter().any(|seg| seg.rect.1 != first.rect.1);
    // Only tabs on screen are targets: a pointer past the last one shown
    // (onto a scroll arrow or `+`) stops there rather than at a hidden tab.
    let mut last_shown = None;
    for seg in segments.iter().filter(|seg| tab_shown(seg)) {
        last_shown = Some(seg.idx);
        let (x, y, w, h) = seg.rect;
        let (cursor, end) = if vertical {
            (cursor_y, y + h)
        } else {
            (cursor_x, x + w)
        };
        if cursor < end {
            return seg.idx;
        }
    }
    last_shown
        .or_else(|| segments.last().map(|seg| seg.idx))
        .unwrap_or(0)
}

/// Pixel distance a held tab press must move before Kettle promotes the click
/// into the in-window reorder gesture. This is intentionally larger than the
/// low-level detachable-tab FSM threshold: a tab click should stay visually
/// anchored to the selected segment through normal hand jitter, while a real
/// drag still starts quickly at typical tab-bar heights.
fn tab_reorder_drag_threshold_px(tab_bar_h: f32) -> f32 {
    (tab_bar_h * 0.5).clamp(8.0, 16.0)
}

/// Pixel distance a held per-pane-titlebar press must move before it becomes a
/// pane drag rather than a click.
///
/// A flat constant, unlike [`tab_reorder_drag_threshold_px`], because the
/// competing gesture is different: a tab press competes with a reorder along a
/// strip whose height sets the natural scale, while a titlebar press competes
/// with "click again to rename". Renaming is the destructive-to-undo one, so the
/// slop is generous enough that ordinary hand jitter during a double click never
/// picks the pane up.
const PANE_DRAG_THRESHOLD_PX: f32 = 12.0;

/// Euclidean distance from a point to the nearest
/// edge of a rect — `0.0` when the point is inside. The tear decision is
/// "distance from the tab band ≥ threshold", which gives UNIFORM hysteresis
/// in every direction away from the band (the Chromium model): drag along
/// the strip = reorder, drag perpendicular past the slop = tear. Because a
/// `Top` band's upper edge coincides with the window's top edge, "above the
/// window" falls out of the same math — no separate outside-the-window
/// clause needed.
fn dist_to_rect(cx: f32, cy: f32, rect: (f32, f32, f32, f32)) -> f32 {
    let (rx, ry, rw, rh) = rect;
    let dx = (rx - cx).max(cx - (rx + rw)).max(0.0);
    let dy = (ry - cy).max(cy - (ry + rh)).max(0.0);
    (dx * dx + dy * dy).sqrt()
}

/// Has the cursor moved far enough from the tab
/// band to tear the dragged tab off into its own window? Pure so the
/// per-orientation cases are unit-testable without a window.
fn tear_threshold_crossed(cx: f32, cy: f32, band: (f32, f32, f32, f32), threshold: f32) -> bool {
    threshold > 0.0 && dist_to_rect(cx, cy, band) >= threshold
}

/// How far (0.0..=1.0) the cursor has travelled from
/// the tab band toward the tear threshold. 0.0 at/inside the band, 1.0 at
/// (or past) the exact distance `tear_threshold_crossed` fires at, so the
/// ghost's visual lift saturates precisely where the tear happens. A
/// non-positive threshold can never tear, so it also never lifts. Pure for
/// the same unit-testability reason as `tear_threshold_crossed`.
fn tear_lift_ratio(cx: f32, cy: f32, band: (f32, f32, f32, f32), threshold: f32) -> f32 {
    if threshold <= 0.0 {
        return 0.0;
    }
    (dist_to_rect(cx, cy, band) / threshold).clamp(0.0, 1.0)
}

/// Re-dock insertion index for a torn-off tab dropped at
/// `cursor` (main-axis coordinate) given the existing segments' main-axis
/// midpoints, in order. Negative midpoints mark hidden segments.
/// First visible midpoint past the cursor wins; otherwise insert after the
/// last visible tab. Distinct from `tab_drag_target_index`
/// (which picks the segment UNDER the cursor for reorder): docking inserts
/// BETWEEN segments, so a strip of n tabs has n+1 valid slots.
fn dock_insertion_index(seg_mids: &[f32], cursor: f32) -> usize {
    let mut insertion = 0;
    for (idx, &mid) in seg_mids.iter().enumerate() {
        if mid < 0.0 {
            continue;
        }
        if cursor < mid {
            return idx;
        }
        insertion = idx + 1;
    }
    insertion
}

/// The tab index a torn-off tab dropped at `cursor` (main-axis coordinate)
/// lands at, among the tabs on screen: before the first whose midpoint is
/// past the cursor, else right after the last one shown, never after tabs
/// scrolled out of view. `None` for a bar with no tab on screen.
fn dock_insertion_tab(
    segments: &[kettle_render::TabSeg],
    vertical: bool,
    cursor: f32,
) -> Option<usize> {
    let shown: Vec<&kettle_render::TabSeg> = segments.iter().filter(|seg| tab_shown(seg)).collect();
    let mids: Vec<f32> = shown
        .iter()
        .map(|seg| {
            let (x, y, w, h) = seg.rect;
            if vertical { y + h * 0.5 } else { x + w * 0.5 }
        })
        .collect();
    let slot = dock_insertion_index(&mids, cursor);
    match shown.get(slot) {
        Some(seg) => Some(seg.idx),
        None => shown.last().map(|seg| seg.idx + 1),
    }
}

/// Width of the strip that horizontal tab segments tile across, given the
/// surface width and the trailing new-tab button geometry.
///
/// `tab_bar()` lays segments out across this width, and the drag-to-reorder
/// handler hit-tests those rendered segments, so the drag target can't drift
/// from what is drawn. `arrow_w` is `0.0` when the dropdown is absent
/// (vertical bars). Floored at `plus_w` so a very narrow bar still reserves room
/// for the `+` button.
fn tab_segment_strip_width(surface_w: f32, plus_w: f32, arrow_w: f32) -> f32 {
    (surface_w - plus_w - arrow_w).max(plus_w)
}

/// The shortest a focus change waits before reading the update facts again.
const UPDATE_FACTS_THROTTLE: std::time::Duration = std::time::Duration::from_secs(10);

/// The ✕ hit zone for one tab segment, or an EMPTY rect when the button is
/// configured off.
///
/// With `close-button-on-tab = false`, a full-size hit rect would still be
/// probed by the press handler, the `ctl` press handler, and the pointer-cursor
/// hover test. Clicking a tab's trailing square would then close it through an
/// invisible button, killing every pane in that tab. A zero-size rect fixes all
/// three, because every consumer tests with a strict `px < rx + rw`, which no
/// zero-width rect can satisfy. `tab_title_rect` reads `close.0` only when the
/// button is visible, so the reclaimed label space is unaffected.
fn tab_close_rect(
    trailing_x: f32,
    y: f32,
    height: f32,
    close_visible: bool,
) -> kettle_render::Rect4 {
    if close_visible {
        (trailing_x, y, height, height)
    } else {
        (trailing_x, y, 0.0, 0.0)
    }
}

/// Text lane inside a tab segment. The close button is visible chrome, so the
/// label must be centered in the lane that remains before that button rather
/// than centered against the full segment and visually drifting under `✕`.
fn tab_title_rect(
    segment: kettle_render::Rect4,
    close: kettle_render::Rect4,
    close_visible: bool,
) -> kettle_render::Rect4 {
    let (x, y, w, h) = segment;
    let left = x + 8.0;
    let right = if close_visible { close.0 } else { x + w - 8.0 };
    (left, y, (right - left).max(0.0), h)
}

/// Should the new-tab `▾` dropdown arrow be shown? Always yes. Besides the
/// shells, the dropdown carries Settings / Command palette / About rows
/// (Windows Terminal's bottom section), so it is never a pointless one-item
/// menu, even on a bash-only Ubuntu.
fn new_tab_dropdown_visible() -> bool {
    true
}

/// Letters the menu's vim navigation layer (`vim-menu-nav`) consumes
/// (`g`/`G` first/last, `h` back, `j`/`k` move, `l` activate). While the
/// setting is on, `assign_mnemonics` must not hand any of these to a row.
/// The nav layer intercepts them before mnemonic dispatch, so a row keyed on
/// one would silently lose its hotkey.
const VIM_NAV_RESERVED: &[char] = &['g', 'h', 'j', 'k', 'l'];

/// Display-column budget for one context-menu item, kept in lockstep with the
/// row constructed by `context_menu_overlay` and the renderer's
/// `menu_row_chars`. Centralizing this prevents the initial anchor clamp,
/// resize/DPI reclamp, hit testing, ellipsis, and draw width from disagreeing.
fn context_menu_item_columns(item: &ContextMenuItem, hint: &str) -> usize {
    let (label, suffix, shows_hint): (&str, &str, bool) = match item {
        ContextMenuItem::Item { label, .. } => (label, "", true),
        ContextMenuItem::DynamicItem { label, .. } => (label, "", true),
        ContextMenuItem::LuaItem { label, .. } => (label, "", false),
        ContextMenuItem::ConfigItem { label, .. } => (label, "", false),
        ContextMenuItem::Submenu { label, .. } => (label, " ▸", false),
        ContextMenuItem::ProfileChoice { label, .. }
        | ContextMenuItem::WindowChoice { label, .. } => (label, "", false),
        ContextMenuItem::NewTabShell { label, .. } => (label, "", true),
        ContextMenuItem::UrlItem { label, .. } => (label, "", false),
        ContextMenuItem::Media { label, .. } | ContextMenuItem::Preview { label, .. } => {
            (label, "", false)
        }
        ContextMenuItem::Info { label } => (label, "", false),
        ContextMenuItem::Separator => return 0,
    };
    label.width()
        + suffix.width()
        + if shows_hint && !hint.is_empty() {
            hint.width() + 2
        } else {
            0
        }
}

/// Compute mnemonics for the context-menu rows. For each row, returns
/// `Some((byte_index, char))` where `char` is the first lowercase A-Z
/// letter in the label that hasn't already been claimed by an earlier row,
/// or `None` for rows without any A-Z (separators, choice rows with no
/// label letters, etc.).
///
/// First-letter is the canonical priority (matches GTK / Win32);
/// fall through to subsequent letters only if the first letter is
/// already taken by an earlier row. Pure so the collision rules
/// are unit-tested without spinning up App.
fn assign_mnemonics(items: &[ContextMenuItem], reserved: &[char]) -> Vec<Option<(usize, char)>> {
    let labels: Vec<&str> = items
        .iter()
        .map(|it| match it {
            ContextMenuItem::Item { label, .. } => *label,
            ContextMenuItem::DynamicItem { label, .. } => label.as_str(),
            ContextMenuItem::LuaItem { label, .. } => label.as_str(),
            ContextMenuItem::ConfigItem { label, .. } => label.as_str(),
            ContextMenuItem::Submenu { label, .. } => label.as_str(),
            ContextMenuItem::ProfileChoice { label, .. }
            | ContextMenuItem::WindowChoice { label, .. } => label.as_str(),
            ContextMenuItem::NewTabShell { label, .. } => label.as_str(),
            ContextMenuItem::UrlItem { label, .. } => *label,
            ContextMenuItem::Media { label, .. } | ContextMenuItem::Preview { label, .. } => *label,
            // Info rows are non-dispatchable — no mnemonic to claim.
            ContextMenuItem::Info { .. } => "",
            ContextMenuItem::Separator => "",
        })
        .collect();
    let mut claimed: std::collections::HashSet<char> = std::collections::HashSet::new();
    let mut out: Vec<Option<(usize, char)>> = vec![None; labels.len()];
    // Two rounds. The stable core rows claim their letters first, then the
    // context-dependent UrlItem rows (present only when the right-click landed
    // on a link) claim from what's left. Otherwise "Open Link" / "Copy Link
    // Address", which lead the menu, would take 'o'/'c' and silently remap
    // muscle-memory mnemonics ('p' would fire Copy instead of Paste).
    let round = |items: &[ContextMenuItem], idx: usize| -> usize {
        usize::from(matches!(
            items[idx],
            ContextMenuItem::UrlItem { .. }
                | ContextMenuItem::Media { .. }
                | ContextMenuItem::Preview { .. }
        ))
    };
    for pass in 0..2 {
        for (i, label) in labels.iter().enumerate() {
            if round(items, i) != pass {
                continue;
            }
            let mut chosen: Option<(usize, char)> = None;
            for (bi, c) in label.char_indices() {
                if !c.is_ascii_alphabetic() {
                    continue;
                }
                let low = c.to_ascii_lowercase();
                // Letters owned by vim-menu-nav are never assignable.
                if reserved.contains(&low) {
                    continue;
                }
                if !claimed.contains(&low) {
                    claimed.insert(low);
                    chosen = Some((bi, low));
                    break;
                }
            }
            out[i] = chosen;
        }
    }
    out
}

/// Match the user's typeahead buffer to a row by
/// case-insensitive prefix on the label. Returns the first
/// dispatchable row whose label (lowercased) starts with `buf`
/// (also lowercased). Separators/empty labels are skipped.
fn typeahead_match(items: &[ContextMenuItem], buf: &str) -> Option<usize> {
    if buf.is_empty() {
        return None;
    }
    let needle = buf.to_ascii_lowercase();
    items.iter().position(|it| match it {
        ContextMenuItem::Item {
            label,
            enabled: true,
            ..
        } => label.to_ascii_lowercase().starts_with(&needle),
        ContextMenuItem::DynamicItem {
            label,
            enabled: true,
            ..
        } => label.to_ascii_lowercase().starts_with(&needle),
        ContextMenuItem::LuaItem { label, .. }
        | ContextMenuItem::ConfigItem { label, .. }
        | ContextMenuItem::Submenu { label, .. }
        | ContextMenuItem::WindowChoice { label, .. }
        | ContextMenuItem::ProfileChoice { label, .. }
        | ContextMenuItem::NewTabShell { label, .. } => {
            label.to_ascii_lowercase().starts_with(&needle)
        }
        ContextMenuItem::UrlItem { label, .. }
        | ContextMenuItem::Media { label, .. }
        | ContextMenuItem::Preview { label, .. } => label.to_ascii_lowercase().starts_with(&needle),
        // Info rows aren't dispatchable, so typeahead skips them.
        _ => false,
    })
}

/// How many context-menu rows starting at
/// `start` fit within `panel_h` pixels. Separators take `sep_h`,
/// every other row takes `row_h`. Used by `step_context_menu_highlight`
/// and `scroll_context_menu` to keep `scroll_offset` honest when the
/// panel is height-clamped by `context_menu_geometry`. Pure so the
/// arithmetic is drift-guarded without standing up the App.
fn count_rows_fitting(
    items: &[ContextMenuItem],
    start: usize,
    panel_h: f32,
    row_h: f32,
    sep_h: f32,
) -> usize {
    let mut used = 0.0_f32;
    let mut count = 0;
    for it in items.iter().skip(start) {
        let h = context_menu_item_height(it, row_h, sep_h);
        if used + h > panel_h {
            break;
        }
        used += h;
        count += 1;
    }
    count
}

fn context_menu_item_height(item: &ContextMenuItem, row_h: f32, sep_h: f32) -> f32 {
    if matches!(item, ContextMenuItem::Separator) {
        sep_h
    } else {
        row_h
    }
}

/// Earliest suffix that fits through the final row. Scrolling past it would
/// leave avoidable blank space below the menu. This is O(items) even for the
/// 512-entry theme submenu.
fn context_menu_max_scroll_offset(
    items: &[ContextMenuItem],
    panel_h: f32,
    row_h: f32,
    sep_h: f32,
) -> usize {
    if items.is_empty() {
        return 0;
    }
    let mut suffix_height = 0.0;
    let mut max_offset = items.len() - 1;
    for candidate in (0..items.len()).rev() {
        let next = suffix_height + context_menu_item_height(&items[candidate], row_h, sep_h);
        if next > panel_h {
            break;
        }
        suffix_height = next;
        max_offset = candidate;
    }
    max_offset
}

fn context_menu_scroll_for_highlight(
    items: &[ContextMenuItem],
    current_offset: usize,
    highlight: usize,
    panel_h: f32,
    row_h: f32,
    sep_h: f32,
) -> usize {
    if items.is_empty() {
        return 0;
    }
    let highlight = highlight.min(items.len() - 1);
    let max_offset = context_menu_max_scroll_offset(items, panel_h, row_h, sep_h);
    let mut offset = current_offset.min(max_offset);
    let visible = count_rows_fitting(items, offset, panel_h, row_h, sep_h);
    if highlight < offset {
        offset = highlight;
    } else if highlight >= offset.saturating_add(visible) {
        // Pull back until the highlight is the last fully visible row. This
        // keeps Enter bound to a visible choice after a window/DPI shrink.
        let mut used = 0.0;
        offset = highlight;
        for candidate in (0..=highlight).rev() {
            let next = used + context_menu_item_height(&items[candidate], row_h, sep_h);
            if next > panel_h {
                break;
            }
            used = next;
            offset = candidate;
        }
    }
    offset.min(max_offset)
}

fn ellipsize_menu_label(label: &str, max_columns: usize) -> String {
    use unicode_segmentation::UnicodeSegmentation as _;

    if label.width() <= max_columns {
        return label.to_string();
    }
    if max_columns == 0 {
        return String::new();
    }
    if max_columns == 1 {
        return "…".to_string();
    }
    let content_columns = max_columns - 1;
    let mut used: usize = 0;
    let mut output = String::new();
    for grapheme in label.graphemes(true) {
        let width = grapheme.width();
        if used.saturating_add(width) > content_columns {
            break;
        }
        output.push_str(grapheme);
        used += width;
    }
    output.push('…');
    output
}

/// Drop disabled rows from the context menu and collapse the separators
/// they would orphan. Hiding them instead of greying them out matches
/// Terminator / GNOME Terminal, which show only what you can click.
///
/// Three passes:
///   1. drop any disabled `Item` or `DynamicItem`. Other variants (LuaItem,
///      ConfigItem, Submenu, Theme/ProfileChoice, Separator) stay.
///   2. collapse runs of `Separator` to a single one.
///   3. trim leading + trailing separators (orphaned by step 1).
///
/// Pure so the contract is unit-tested without spinning up App.
fn filter_disabled(items: Vec<ContextMenuItem>) -> Vec<ContextMenuItem> {
    // Step 1: drop disabled Items.
    let kept: Vec<ContextMenuItem> = items
        .into_iter()
        .filter(|it| {
            !matches!(
                it,
                ContextMenuItem::Item { enabled: false, .. }
                    | ContextMenuItem::DynamicItem { enabled: false, .. }
            )
        })
        .collect();
    // Step 2: collapse separator runs. Walk linearly; only push a
    // Separator when the previous pushed item wasn't already one.
    let mut collapsed: Vec<ContextMenuItem> = Vec::with_capacity(kept.len());
    let mut last_was_sep = true; // pretend the "before-start" was a sep so leading sep gets dropped
    for it in kept {
        let is_sep = matches!(it, ContextMenuItem::Separator);
        if is_sep && last_was_sep {
            continue;
        }
        last_was_sep = is_sep;
        collapsed.push(it);
    }
    // Step 3: trim a trailing separator (orphan).
    if let Some(ContextMenuItem::Separator) = collapsed.last() {
        collapsed.pop();
    }
    collapsed
}

/// Terminator menu UX, hover-to-highlight. Walk the
/// vertical pixel layout of a context-menu's rows and return the
/// row index containing `cursor_y`, or `None` if the cursor landed
/// on a separator (visual gap) or beyond the last row. Pure so the
/// arithmetic + the separator-skip contract is unit-tested without
/// standing up an App + a winit window. A `true` item flags a separator (uses
/// `sep_h` instead of `row_h`). Accepting an iterator lets the live pointer path
/// project item kinds without allocating a temporary vector.
pub(crate) fn find_menu_row_y(
    cursor_y: f32,
    anchor_y: f32,
    panel_h: f32,
    row_h: f32,
    sep_h: f32,
    kinds: impl IntoIterator<Item = bool>,
) -> Option<usize> {
    let mut row_y = anchor_y;
    let panel_bottom = anchor_y + panel_h.max(0.0);
    for (idx, is_sep) in kinds.into_iter().enumerate() {
        let h = if is_sep { sep_h } else { row_h };
        // The renderer draws only complete rows. A clamped panel can leave a
        // short blank strip at the bottom; never dispatch the next invisible
        // row through that strip.
        if row_y + h > panel_bottom {
            break;
        }
        if cursor_y >= row_y && cursor_y < row_y + h {
            return if is_sep { None } else { Some(idx) };
        }
        row_y += h;
    }
    None
}

/// Terminator parity (`layoutlauncher.py`): rank saved
/// layouts against the user-typed query. Empty query returns
/// every layout in original (alphabetical) order; non-empty query
/// keeps only entries whose lower-cased name contains every
/// lower-cased query token. Same shape as
/// `kettle_config::palette::rank` but layouts have only a name
/// field (no description), so the inner predicate is simpler.
/// Pure and separate from `layout_picker_key` so a drift guard
/// can exercise it without touching App state.
pub(crate) fn rank_layouts(q: &str, layouts: &[String]) -> Vec<usize> {
    let q = q.trim().to_ascii_lowercase();
    if q.is_empty() {
        return (0..layouts.len()).collect();
    }
    let tokens: Vec<&str> = q.split_whitespace().collect();
    layouts
        .iter()
        .enumerate()
        .filter(|(_, name)| {
            let lower = name.to_ascii_lowercase();
            tokens.iter().all(|t| lower.contains(t))
        })
        .map(|(i, _)| i)
        .collect()
}

/// Indices into the configured SSH-host list, best fuzzy match first. Ties
/// preserve config order so an empty query has a stable initial selection.
fn rank_ssh_hosts(query: &str, hosts: &[(String, String)]) -> Vec<usize> {
    let query = query.trim();
    let mut ranked: Vec<(usize, i32)> = hosts
        .iter()
        .enumerate()
        .filter_map(|(index, (name, _))| {
            kettle_config::fuzzy::score(query, name).map(|score| (index, score))
        })
        .collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    ranked.into_iter().map(|(index, _)| index).collect()
}

fn selected_ssh_target(query: &str, selected: usize, hosts: &[(String, String)]) -> Option<String> {
    let ranked = rank_ssh_hosts(query, hosts);
    let selected = selected.min(ranked.len().saturating_sub(1));
    ranked
        .get(selected)
        .map(|&index| hosts[index].1.clone())
        .or_else(|| {
            let query = query.trim();
            (!query.is_empty()).then(|| query.to_string())
        })
}

struct PickerList {
    rows: Vec<ContextMenuRow>,
    selected: usize,
}

/// A picker's key hint: each key followed by what it does. The keys are the
/// same in every language.
fn picker_hint(tr: &kettle_i18n::Translator, hints: &[(&str, kettle_i18n::Text)]) -> String {
    let parts: Vec<String> = hints
        .iter()
        .map(|(keys, what)| format!("{keys} {}", tr.text(*what)))
        .collect();
    format!("({})", parts.join(" · "))
}

const PICKER_MAX_VISIBLE_ROWS: usize = 8;

/// The palette's rows: every command, then, while an update notice is
/// active, its release notes and Dismiss.
fn palette_entries(
    notice: Option<&crate::update_notice::Notice>,
    tr: &kettle_i18n::Translator,
) -> Vec<(kettle_config::palette::PaletteLabel, Action)> {
    use kettle_config::palette::PaletteLabel;
    let mut entries: Vec<_> = kettle_config::palette::commands()
        .into_iter()
        .map(|(label, action)| (PaletteLabel::Text(label), action))
        .collect();
    if let Some(notice) = notice {
        let english = kettle_i18n::Translator::new(kettle_i18n::Language::En);
        entries.push((
            PaletteLabel::Dynamic {
                shown: tr.palette_open_update(&notice.tag),
                english: english.palette_open_update(&notice.tag),
            },
            Action::OpenUpdate,
        ));
        entries.push((
            PaletteLabel::Text(kettle_i18n::Text::PaletteDismissUpdate),
            Action::DismissUpdate,
        ));
    }
    entries
}

fn command_picker_list(
    query: &str,
    selected: usize,
    bindings: &Bindings,
    tr: &kettle_i18n::Translator,
    notice: Option<&crate::update_notice::Notice>,
) -> PickerList {
    let commands = palette_entries(notice, tr);
    let ranked = kettle_config::palette::rank_entries(query, &commands, tr);
    let rows = ranked
        .iter()
        .map(|&index| {
            let (label, action) = &commands[index];
            ContextMenuRow {
                label: label.shown(tr).to_string(),
                separator: false,
                enabled: true,
                hint: kettle_config::keybinds::hint_label(bindings, action).unwrap_or_default(),
            }
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        PickerList {
            rows: vec![ContextMenuRow {
                label: tr
                    .text(kettle_i18n::Text::PickerNoMatchingCommand)
                    .to_string(),
                separator: false,
                enabled: false,
                hint: String::new(),
            }],
            selected: 0,
        }
    } else {
        PickerList {
            selected: selected.min(rows.len() - 1),
            rows,
        }
    }
}

fn layout_picker_list(
    query: &str,
    selected: usize,
    layouts: &[String],
    tr: &kettle_i18n::Translator,
) -> PickerList {
    let ranked = rank_layouts(query, layouts);
    let rows = ranked
        .iter()
        .map(|&index| ContextMenuRow {
            label: layouts[index].clone(),
            separator: false,
            enabled: true,
            hint: String::new(),
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        let label = if layouts.is_empty() {
            kettle_i18n::Text::PickerNoSavedLayouts
        } else {
            kettle_i18n::Text::PickerNoMatchingLayout
        };
        PickerList {
            rows: vec![ContextMenuRow {
                label: tr.text(label).to_string(),
                separator: false,
                enabled: false,
                hint: if layouts.is_empty() {
                    tr.text(kettle_i18n::Text::PickerCreateLayoutHint)
                        .to_string()
                } else {
                    String::new()
                },
            }],
            selected: 0,
        }
    } else {
        PickerList {
            selected: selected.min(rows.len() - 1),
            rows,
        }
    }
}

/// The themes the theme picker lists for `query`, each with whether it is
/// dark. An empty query lists every theme of `opened_on`'s appearance in look
/// order, then the other appearance, so the arrows step between similar
/// palettes and reach the other appearance only at the end. A query ranks by
/// fuzzy match, ties keeping that order, except that a theme named exactly by
/// the query comes first: `dracula` scores the same against "Dracula+".
fn theme_picker_matches(query: &str, opened_on: &str) -> Vec<(&'static str, bool)> {
    let opened_dark = kettle_config::Theme::by_name(opened_on).is_dark();
    let order = [opened_dark, !opened_dark].into_iter().flat_map(|dark| {
        kettle_config::Theme::look_order(dark)
            .iter()
            .map(move |name| (*name, dark))
    });
    let query = query.trim();
    if query.is_empty() {
        return order.collect();
    }
    let mut ranked: Vec<(bool, i32, usize, (&'static str, bool))> = order
        .enumerate()
        .filter_map(|(index, theme)| {
            let exact = theme.0.eq_ignore_ascii_case(query);
            kettle_config::fuzzy::score(query, theme.0).map(|score| (exact, score, index, theme))
        })
        .collect();
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.cmp(&a.1)).then(a.2.cmp(&b.2)));
    ranked.into_iter().map(|(_, _, _, theme)| theme).collect()
}

/// The theme picker's rows. The theme it opened on is ticked, and each row
/// says whether it is dark or light.
fn theme_picker_list(
    query: &str,
    selected: usize,
    opened_on: &str,
    tr: &kettle_i18n::Translator,
) -> PickerList {
    use kettle_i18n::Text;
    let rows: Vec<ContextMenuRow> = theme_picker_matches(query, opened_on)
        .into_iter()
        .map(|(name, dark)| {
            let appearance = tr.text(if dark {
                Text::PickerThemeDark
            } else {
                Text::PickerThemeLight
            });
            let tick = if name.eq_ignore_ascii_case(opened_on.trim()) {
                "✓ "
            } else {
                ""
            };
            ContextMenuRow {
                label: name.to_string(),
                separator: false,
                enabled: true,
                hint: format!("{tick}{appearance}"),
            }
        })
        .collect();
    if rows.is_empty() {
        return PickerList {
            rows: vec![ContextMenuRow {
                label: tr.text(Text::PickerNoMatchingTheme).to_string(),
                separator: false,
                enabled: false,
                hint: String::new(),
            }],
            selected: 0,
        };
    }
    let selected = selected.min(rows.len() - 1);
    PickerList { rows, selected }
}

/// The theme the picker's highlighted row names, if any row matches. Clamped
/// like the painted highlight, so the preview and Enter use the row shown.
fn theme_picker_selection(state: &crate::window_state::ThemePickerState) -> Option<&'static str> {
    let matches = theme_picker_matches(&state.query, &state.opened_on);
    let last = matches.len().checked_sub(1)?;
    Some(matches[state.selected.min(last)].0)
}

/// A theme the window can wear: its name and colours.
type ThemeState = (String, kettle_config::Theme);

/// Open the theme picker on `live`, the theme the window wears, and keep it as
/// the baseline Esc restores. A preview still running from an earlier picker
/// is cancelled first, so the new picker opens on, ticks and restores the
/// theme from before that preview. Returns the theme to put back, if any.
fn begin_theme_picker(ws: &mut WindowState, live: ThemeState) -> Option<ThemeState> {
    let restore = ws.theme_preview.take();
    let opened = restore.clone().unwrap_or(live);
    ws.theme_picker_input = Some(crate::window_state::ThemePickerState {
        query: String::new(),
        selected: theme_picker_start(&opened.0),
        opened_on: opened.0.clone(),
    });
    ws.theme_preview = Some(opened);
    restore
}

fn begin_theme_picker_session(
    ws: &mut WindowState,
    windows: &mut std::collections::BTreeMap<u64, WindowState>,
    live: ThemeState,
) -> Option<ThemeState> {
    for other in windows.values_mut() {
        if let Some(baseline) = other.theme_preview.take() {
            ws.theme_preview = Some(baseline);
        }
        if other.theme_picker_input.take().is_some() {
            end_modal_composition(other);
            if let Some(window) = &other.window {
                window.request_redraw();
            }
        }
    }
    begin_theme_picker(ws, live)
}

/// The theme the window must switch to after an event, if any, while
/// `live_name` is the one it wears. An open picker shows its selection, or its
/// baseline while nothing matches the query; the baseline stays until the
/// picker closes. A picker that closed without keeping a theme (a kept one
/// clears the baseline first) puts the baseline back.
fn theme_preview_change(ws: &mut WindowState, cfg: &Config) -> Option<ThemeState> {
    let live_name = cfg.theme_name.as_str();
    let Some(state) = ws.theme_picker_input.as_ref() else {
        return ws.theme_preview.take();
    };
    match theme_picker_selection(state) {
        Some(name) if name == live_name => None,
        Some(name) => Some((name.to_string(), cfg.theme_by_name(name))),
        None => ws
            .theme_preview
            .as_ref()
            .filter(|(name, _)| name != live_name)
            .cloned(),
    }
}

/// End an input-method composition when the modal that owned it closes (see
/// `remember_closing_key`). A late Commit no longer matches the focus
/// generation, so it is dropped instead of typed into whatever the modal
/// covered.
fn end_modal_composition(ws: &mut WindowState) {
    ws.ime_focus_generation = ws.ime_focus_generation.wrapping_add(1);
    if let Some(session) = ws.ime_preedit_owner.as_mut() {
        ws.ime_preedit = None;
        session.ended.get_or_insert(ws.ime_focus_generation);
    }
}

/// Where the theme picker opens: on the current theme, so the first arrow
/// press steps to its nearest look.
fn theme_picker_start(current: &str) -> usize {
    theme_picker_matches("", current)
        .iter()
        .position(|(name, _)| name.eq_ignore_ascii_case(current.trim()))
        .unwrap_or(0)
}

fn ssh_picker_list(
    query: &str,
    selected: usize,
    hosts: &[(String, String)],
    tr: &kettle_i18n::Translator,
) -> PickerList {
    let ranked = rank_ssh_hosts(query, hosts);
    let rows = ranked
        .iter()
        .map(|&index| ContextMenuRow {
            label: hosts[index].0.clone(),
            separator: false,
            enabled: true,
            hint: String::new(),
        })
        .collect::<Vec<_>>();
    if rows.is_empty() {
        PickerList {
            rows: vec![ContextMenuRow {
                label: tr
                    .text(if hosts.is_empty() {
                        kettle_i18n::Text::PickerNoConfiguredHosts
                    } else {
                        kettle_i18n::Text::PickerNoMatchingHost
                    })
                    .to_string(),
                separator: false,
                enabled: false,
                hint: tr.text(kettle_i18n::Text::PickerTypedTarget).to_string(),
            }],
            selected: 0,
        }
    } else {
        PickerList {
            selected: selected.min(rows.len() - 1),
            rows,
        }
    }
}

/// Keep the selected picker row inside the visible suffix. Picker rows have no
/// separators, so this is the context-menu scroll rule reduced to equal-height
/// rows and can be recomputed from selection on each frame.
fn picker_scroll_offset(
    row_count: usize,
    selected: usize,
    panel_height: f32,
    row_height: f32,
) -> usize {
    if row_count == 0
        || !panel_height.is_finite()
        || !row_height.is_finite()
        || panel_height <= 0.0
        || row_height <= 0.0
    {
        return 0;
    }
    let visible = ((panel_height / row_height).floor() as usize)
        .max(1)
        .min(row_count);
    let selected = selected.min(row_count - 1);
    selected
        .saturating_add(1)
        .saturating_sub(visible)
        .min(row_count - visible)
}

/// Place a picker list above its bottom input lane using the same rows, width
/// budget, clipping, selection rail, and overflow cues as the context menu.
fn picker_context_menu(
    mut list: PickerList,
    surface: (f32, f32),
    cell: (f32, f32),
) -> Option<ContextMenu> {
    let (surface_w, surface_h) = surface;
    let (cell_w, cell_h) = cell;
    let input_height = cell_h + 10.0;
    let list_surface = (surface_w, (surface_h - input_height - 4.0).max(0.0));
    if !context_menu_surface_can_fit_row(list_surface, cell) || list.rows.is_empty() {
        return None;
    }

    let row_height = cell_h + kettle_render::menu::ROW_PAD;
    let max_columns = list
        .rows
        .iter()
        .map(kettle_render::menu_row_chars)
        .max()
        .unwrap_or(0) as f32;
    let natural_width = kettle_render::menu::panel_width(max_columns, cell_w);
    let natural_height = row_height * list.rows.len().min(PICKER_MAX_VISIBLE_ROWS) as f32;
    let panel = clamp_context_menu_panel((natural_width, natural_height), list_surface);
    let max_columns = kettle_render::menu::panel_columns(panel.0, cell_w);
    for row in &mut list.rows {
        fit_context_menu_row(row, max_columns);
    }
    let scroll_offset = picker_scroll_offset(list.rows.len(), list.selected, panel.1, row_height);
    let requested_anchor = (
        (surface_w - panel.0) * 0.5,
        (list_surface.1 - panel.1 - 4.0).max(4.0),
    );
    let anchor = clamp_context_menu_anchor(requested_anchor, panel, list_surface);
    Some(ContextMenu {
        anchor,
        rows: list.rows,
        highlight: list.selected,
        scroll_offset,
        panel_w_clamped: panel.0,
        panel_h_clamped: panel.1,
        a11y_label: None,
    })
}

/// The open picker's rows as a menu panel: the first of `pickers` that is
/// open, in key-routing order.
fn picker_overlay_context_menu(
    pickers: [Option<PickerList>; 4],
    surface: (f32, f32),
    cell: (f32, f32),
) -> Option<ContextMenu> {
    pickers
        .into_iter()
        .flatten()
        .next()
        .and_then(|list| picker_context_menu(list, surface, cell))
}

/// The `vim-menu-nav` `Ctrl+d`/`Ctrl+u` half-page target. Moves
/// `current` by `rows` items in `dir` (no wrap — vim half-page semantics
/// clamp at the ends), then snaps to the nearest dispatchable row in the
/// direction of travel (falling back to the other direction so a
/// trailing separator can't strand the highlight). Pure so the
/// clamp+snap math is unit-testable without App state.
fn half_page_menu_target(
    items: &[ContextMenuItem],
    current: usize,
    rows: usize,
    dir: isize,
) -> usize {
    if items.is_empty() {
        return current;
    }
    let n = items.len() as isize;
    let step = rows.max(1) as isize;
    let raw = (current as isize + dir * step).clamp(0, n - 1) as usize;
    let scan_down = |from: usize| (from..items.len()).find(|&i| item_is_dispatchable(&items[i]));
    let scan_up = |from: usize| (0..=from).rev().find(|&i| item_is_dispatchable(&items[i]));
    let snapped = if dir > 0 {
        scan_down(raw).or_else(|| scan_up(raw))
    } else {
        scan_up(raw).or_else(|| scan_down(raw))
    };
    snapped.unwrap_or(current)
}

/// Whether keyboard nav can land on `item` and Enter / click can dispatch it.
fn item_is_dispatchable(item: &ContextMenuItem) -> bool {
    matches!(
        item,
        ContextMenuItem::Item { enabled: true, .. }
            | ContextMenuItem::DynamicItem { enabled: true, .. }
            | ContextMenuItem::LuaItem { .. }
            | ContextMenuItem::ConfigItem { .. }
            // Submenu rows are dispatchable for keyboard
            // nav (↑↓ lands on them); click / Enter drills in.
            | ContextMenuItem::Submenu { .. }
            // Profile choice leaves are the *contents* of a drilled-in
            // Profile ▸ submenu. Without them, keyboard nav inside that
            // submenu could not land on any row or pick a profile.
            | ContextMenuItem::WindowChoice { .. }
            | ContextMenuItem::ProfileChoice { .. }
            // New-tab ▾ shell choices are always clickable + keyboard-
            // navigable.
            | ContextMenuItem::NewTabShell { .. }
            // The URL-aware "Open Link" / "Copy Link Address" rows.
            | ContextMenuItem::UrlItem { .. }
            // A card's or the viewer's media rows.
            | ContextMenuItem::Media { .. }
            | ContextMenuItem::Preview { .. } // ContextMenuItem::Info is deliberately absent: a static info line
                                              // (About panel) is not highlightable or clickable.
    )
}

/// Map a context-menu row at position `idx` (within the current level)
/// to the click it dispatches. Shared by the mouse hit-test
/// ([`App::context_menu_click_action`]) and the keyboard Enter / Space and
/// mnemonic paths so every dispatchable row type (submenu, Lua,
/// config-command, theme/profile choice, new-tab shell) is reachable
/// identically from mouse and keyboard. One mapper keeps the three input
/// paths from drifting apart.
fn item_to_click(item: &ContextMenuItem, idx: usize) -> Option<ContextMenuClick> {
    match item {
        ContextMenuItem::Item {
            action,
            enabled: true,
            ..
        }
        | ContextMenuItem::DynamicItem {
            action,
            enabled: true,
            ..
        } => Some(ContextMenuClick::Action(action.clone())),
        ContextMenuItem::LuaItem { lua_idx, .. } => Some(ContextMenuClick::LuaMenuItem(*lua_idx)),
        ContextMenuItem::ConfigItem {
            command, new_pane, ..
        } => Some(if *new_pane {
            ContextMenuClick::ReconnectRemote(command.clone())
        } else {
            ContextMenuClick::ConfigCommand(command.clone())
        }),
        ContextMenuItem::Submenu { .. } => Some(ContextMenuClick::DrillIntoSubmenu(idx)),
        ContextMenuItem::WindowChoice { window, panes, .. } => {
            Some(ContextMenuClick::MoveTabToWindow {
                window: *window,
                panes: Arc::clone(panes),
            })
        }
        ContextMenuItem::ProfileChoice { profile, .. } => {
            Some(ContextMenuClick::SetProfile(profile.clone()))
        }
        ContextMenuItem::NewTabShell { argv, .. } => {
            Some(ContextMenuClick::NewTabWithArgv(argv.clone()))
        }
        ContextMenuItem::UrlItem { url, how, pane, .. } => Some(ContextMenuClick::Url {
            url: url.clone(),
            how: *how,
            pane: *pane,
        }),
        ContextMenuItem::Media { target, action, .. } => Some(ContextMenuClick::Media {
            target: *target,
            action: *action,
        }),
        ContextMenuItem::Preview { target, action, .. } => Some(ContextMenuClick::Preview {
            target: *target,
            action: *action,
        }),
        ContextMenuItem::Item { enabled: false, .. }
        | ContextMenuItem::DynamicItem { enabled: false, .. }
        | ContextMenuItem::Info { .. }
        | ContextMenuItem::Separator => None,
    }
}

/// Pure: walk the menu item list to find the next enabled, non-
/// separator row index, given a `delta` (±1) and a wrap-around at the
/// list ends. Used by both `↑` and `↓` keyboard nav. Returns `current`
/// unchanged if no enabled rows exist at all (defensive; the menu
/// shouldn't have been opened with zero actionable rows).
fn next_context_menu_highlight(items: &[ContextMenuItem], current: usize, delta: isize) -> usize {
    if items.is_empty() {
        return current;
    }
    let len = items.len() as isize;
    let step = if delta >= 0 { 1 } else { -1 };
    let mut i = current as isize;
    for _ in 0..len {
        i = (i + step).rem_euclid(len);
        if item_is_dispatchable(&items[i as usize]) {
            return i as usize;
        }
    }
    current
}

/// Pure: clamp the requested panel anchor so the panel of size
/// `(panel_w, panel_h)` stays fully inside a surface of size
/// `(surface_w, surface_h)`. A 4-px screen-edge margin so the panel
/// reads as floating rather than glued to the edge. Right-click near
/// the bottom-right corner therefore flips the menu up-and-left
/// instead of rendering off-screen.
fn clamp_context_menu_anchor(
    (req_x, req_y): (f32, f32),
    (panel_w, panel_h): (f32, f32),
    (surface_w, surface_h): (f32, f32),
) -> (f32, f32) {
    let margin = 4.0_f32;
    let max_x = (surface_w - panel_w - margin).max(margin);
    let max_y = (surface_h - panel_h - margin).max(margin);
    let x = req_x.clamp(margin, max_x);
    let y = req_y.clamp(margin, max_y);
    (x, y)
}

fn clamp_context_menu_panel(
    (panel_w, panel_h): (f32, f32),
    (surface_w, surface_h): (f32, f32),
) -> (f32, f32) {
    let available_w = (surface_w - 8.0).max(1.0);
    let available_h = (surface_h - 8.0).max(1.0);
    (panel_w.min(available_w), panel_h.min(available_h))
}

fn context_menu_surface_can_fit_row(
    (surface_w, surface_h): (f32, f32),
    (cell_w, cell_h): (f32, f32),
) -> bool {
    surface_w.is_finite()
        && surface_h.is_finite()
        && cell_w.is_finite()
        && cell_h.is_finite()
        && surface_w >= 8.0 + kettle_render::menu::H_PAD + cell_w.max(1.0)
        && surface_h >= 8.0 + cell_h.max(1.0) + kettle_render::menu::ROW_PAD
}

fn fit_context_menu_row(row: &mut ContextMenuRow, max_columns: usize) {
    if row.separator {
        return;
    }
    let hint_columns = if row.hint.is_empty() {
        0
    } else {
        row.hint.width().saturating_add(2)
    };
    let hint_columns = if hint_columns >= max_columns {
        row.hint.clear();
        0
    } else {
        hint_columns
    };
    row.label = ellipsize_menu_label(&row.label, max_columns.saturating_sub(hint_columns));
}

fn scale_context_menu_anchor(
    (x, y): (f32, f32),
    previous_scale: f64,
    current_scale: f64,
) -> (f32, f32) {
    if !previous_scale.is_finite()
        || previous_scale <= 0.0
        || !current_scale.is_finite()
        || current_scale <= 0.0
    {
        return (x, y);
    }
    let ratio = (current_scale / previous_scale) as f32;
    (x * ratio, y * ratio)
}

/// `(active-tab-index, focused-leaf-id)` — the value `App::focus_key` returns.
pub(crate) type FocusKey = (usize, Option<u64>);
/// Cache key for the viewport link re-scan: `(focus, focused pane's
/// output_generation, its geometry_generation, scroll display_offset,
/// focused cwd)`. It uses the focused pane's generation, not the tab's
/// `last_output_at`, because the activity latch updates that only for
/// background tabs, so active-tab output would never invalidate the scan. The
/// geometry generation covers a resize, which reflows text with no output:
/// zooming a pane, splitting next to it, or resizing the window.
pub(crate) type LinksScanKey = (
    FocusKey,
    Option<u64>,
    Option<u64>,
    Option<usize>,
    Option<String>,
);

/// Re-scan the focused pane's visible links into `ws.links` unless the key
/// shows nothing they depend on has changed. The brief lock to read
/// display_offset is cheap; the avoided work is `kettle_core::links`'
/// per-cell regex pass.
fn scan_links(ws: &mut WindowState) {
    let key: LinksScanKey = {
        let focus = (ws.mux.active, ws.mux.active_focus());
        let focused = ws.mux.focused().map(|pane| &*pane);
        (
            focus,
            // Cheap atomic and short-lock reads.
            focused.map(|p| p.term.output_generation()),
            focused.map(|p| p.term.geometry_generation()),
            focused.and_then(|p| p.term.term.lock().ok().map(|t| t.grid().display_offset())),
            focused.and_then(|p| p.term.current_dir_or_native()),
        )
    };
    if ws.links_scan_key.as_ref() == Some(&key) {
        return;
    }
    // Focused interactive apps like `less -R` redraw visible text by writing
    // output, not by changing terminal scrollback display_offset, and a
    // resize reflows it with no output at all. Re-scan on either so path and
    // URL underline rects stay attached to the current text.
    ws.links = ws
        .mux
        .focused()
        .and_then(|p| {
            p.term
                .term
                .lock()
                .ok()
                .map(|t| kettle_core::links::links_with_cwd(&t, key.4.as_deref()))
        })
        .unwrap_or_default();
    ws.links_scan_key = Some(key);
}

/// How long the transient `cols×rows` chip stays up after the last resize
/// event. Ghostty `resize-overlay` parity, using Ghostty's
/// `resize-overlay-duration` default.
pub(crate) const RESIZE_OVERLAY_DURATION: std::time::Duration =
    std::time::Duration::from_millis(750);
/// How long a pane's visual-bell flash lasts from the frame it rang. One
/// constant for the redraw pacing and the ramp, so the two cannot drift.
pub(crate) const BELL_FLASH_DURATION: std::time::Duration = std::time::Duration::from_millis(300);

/// The visual-bell flash's remaining strength `elapsed` after the bell: 1.0
/// the frame it rang, 0.0 once `BELL_FLASH_DURATION` has passed. Instant on,
/// then an ease-out (quadratic) decay: the peak is what the eye notices, so
/// the wash leaves it quickly and spends the rest of the window in a soft
/// tail rather than lingering near the peak the way a linear ramp does. The
/// renderer scales the configured lightness step by this value, so the fade
/// is perceptually linear as well.
pub(crate) fn bell_flash_ramp(elapsed: std::time::Duration) -> f32 {
    let t = elapsed.as_secs_f32() / BELL_FLASH_DURATION.as_secs_f32();
    if t >= 1.0 {
        return 0.0;
    }
    let remaining = 1.0 - t.max(0.0);
    remaining * remaining
}
const INPUT_REJECTION_NOTICE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);
const MAX_PENDING_LUA_COMMANDS: usize = 1024;
const MAX_PENDING_LUA_SEND_BYTES: usize = 8 << 20;
const MAX_LUA_SEND_TEXT_BYTES: usize = 1 << 20;
const AUTOMATION_WORK_ITEMS_PER_TURN: usize = 16;
const AUTOMATION_WORK_BYTES_PER_TURN: usize = 1 << 20;
const AUTOMATION_RETRY_MIN: std::time::Duration = std::time::Duration::from_millis(10);
const AUTOMATION_RETRY_MAX: std::time::Duration = std::time::Duration::from_millis(250);
const AUTOMATION_YIELD: std::time::Duration = std::time::Duration::from_millis(1);
const MAX_REMOTE_COMMANDS_PER_BATCH: usize = 1024;

#[derive(Debug)]
enum PendingLuaCommand {
    SendText {
        origin_window: u64,
        target_pane: Option<u64>,
        bytes: Arc<[u8]>,
    },
    ExecAction {
        origin_window: u64,
        action: Action,
    },
    Notify {
        origin_window: u64,
        title: String,
        body: String,
    },
    /// Kettle's own notice that it retired runaway Lua callbacks.
    CallbackStopped {
        origin_window: u64,
        event: String,
        dropped: u64,
    },
    SetTheme {
        origin_window: u64,
        name: String,
    },
}

impl PendingLuaCommand {
    fn origin_window(&self) -> u64 {
        match self {
            Self::SendText { origin_window, .. }
            | Self::ExecAction { origin_window, .. }
            | Self::Notify { origin_window, .. }
            | Self::CallbackStopped { origin_window, .. }
            | Self::SetTheme { origin_window, .. } => *origin_window,
        }
    }

    fn send_len(&self) -> usize {
        match self {
            Self::SendText { bytes, .. } => bytes.len(),
            _ => 0,
        }
    }
}

#[derive(Debug, Default)]
struct PendingLuaCommands {
    commands: VecDeque<PendingLuaCommand>,
    send_bytes: usize,
}

#[derive(Debug)]
enum PendingRemoteCommand {
    SendText {
        origin_window: u64,
        target_pane: Option<u64>,
        bytes: Arc<[u8]>,
    },
    ToggleWindow {
        origin_window: u64,
    },
    NewTab {
        origin_window: u64,
    },
}

impl PendingRemoteCommand {
    fn origin_window(&self) -> u64 {
        match self {
            Self::SendText { origin_window, .. }
            | Self::ToggleWindow { origin_window }
            | Self::NewTab { origin_window } => *origin_window,
        }
    }
}

#[derive(Debug)]
enum ParsedRemoteCommandBatch {
    Accepted {
        commands: VecDeque<PendingRemoteCommand>,
        unknown_lines: usize,
    },
    RejectedTooMany {
        operation_count: usize,
        unknown_lines: usize,
    },
}

/// Parse one already byte-bounded spool claim without allowing short control
/// lines to amplify into an unbounded operation queue. The parser scans the
/// full batch to produce one coalesced unknown-line count, but retains at most
/// `MAX_REMOTE_COMMANDS_PER_BATCH` operations. Exceeding that count rejects the
/// whole batch before any retained prefix can be dispatched.
fn parse_remote_command_batch(text: &str, origin_window: u64) -> ParsedRemoteCommandBatch {
    let mut commands = VecDeque::new();
    let mut operation_count = 0usize;
    let mut unknown_lines = 0usize;
    for line in text.lines() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(payload) = line.strip_prefix("send-text-json ") {
            match serde_json::from_str::<String>(payload) {
                Ok(payload) => {
                    operation_count = operation_count.saturating_add(1);
                    if operation_count <= MAX_REMOTE_COMMANDS_PER_BATCH {
                        commands.push_back(PendingRemoteCommand::SendText {
                            origin_window,
                            target_pane: None,
                            bytes: Arc::from(payload.into_bytes()),
                        });
                    }
                }
                Err(_) => {
                    unknown_lines = unknown_lines.saturating_add(1);
                }
            }
        } else if let Some(payload) = line.strip_prefix("send-text ") {
            operation_count = operation_count.saturating_add(1);
            if operation_count <= MAX_REMOTE_COMMANDS_PER_BATCH {
                commands.push_back(PendingRemoteCommand::SendText {
                    origin_window,
                    target_pane: None,
                    bytes: Arc::from(payload.replace("\\n", "\n").into_bytes()),
                });
            }
        } else if line == "toggle-window" {
            operation_count = operation_count.saturating_add(1);
            if operation_count <= MAX_REMOTE_COMMANDS_PER_BATCH {
                commands.push_back(PendingRemoteCommand::ToggleWindow { origin_window });
            }
        } else if line == "new-tab" {
            operation_count = operation_count.saturating_add(1);
            if operation_count <= MAX_REMOTE_COMMANDS_PER_BATCH {
                commands.push_back(PendingRemoteCommand::NewTab { origin_window });
            }
        } else {
            unknown_lines = unknown_lines.saturating_add(1);
        }
    }
    if operation_count > MAX_REMOTE_COMMANDS_PER_BATCH {
        ParsedRemoteCommandBatch::RejectedTooMany {
            operation_count,
            unknown_lines,
        }
    } else {
        ParsedRemoteCommandBatch::Accepted {
            commands,
            unknown_lines,
        }
    }
}

#[derive(Debug, Eq, PartialEq)]
enum RemoteBatchClaim {
    Empty,
    Busy,
    Claimed(String),
    RejectedMalformed,
    RejectedOversize,
}

fn claim_remote_command_file(path: &std::path::Path) -> std::io::Result<RemoteBatchClaim> {
    use std::io::{Read as _, Seek as _};

    let lock_path = kettle_state::remote_command_lock_path(path);
    let Some(_lock) = kettle_state::ExclusiveFileLock::try_acquire(&lock_path)? else {
        return Ok(RemoteBatchClaim::Busy);
    };
    let mut file = kettle_state::open_existing_private_file(path)?;
    let size = file.metadata()?.len();
    if size > kettle_state::MAX_REMOTE_COMMAND_BYTES {
        file.set_len(0)?;
        return Ok(RemoteBatchClaim::RejectedOversize);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    file.seek(std::io::SeekFrom::Start(0))?;
    file.by_ref()
        .take(kettle_state::MAX_REMOTE_COMMAND_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > kettle_state::MAX_REMOTE_COMMAND_BYTES {
        file.set_len(0)?;
        return Ok(RemoteBatchClaim::RejectedOversize);
    }
    if bytes.is_empty() {
        return Ok(RemoteBatchClaim::Empty);
    }
    let text = match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(_) => {
            file.set_len(0)?;
            return Ok(RemoteBatchClaim::RejectedMalformed);
        }
    };
    file.set_len(0)?;
    Ok(RemoteBatchClaim::Claimed(text))
}

fn should_poll_remote_window(
    has_claimed_fifo: bool,
    claim_pending: bool,
    retry_pending: bool,
    window_seq: u64,
    focused_seq: u64,
) -> bool {
    has_claimed_fifo || ((claim_pending || retry_pending) && window_seq == focused_seq)
}

impl PendingLuaCommands {
    fn push(&mut self, command: PendingLuaCommand) -> PaneInputResult {
        let send_len = command.send_len();
        if send_len > MAX_LUA_SEND_TEXT_BYTES {
            return PaneInputResult::Oversize;
        }
        if self.commands.len() >= MAX_PENDING_LUA_COMMANDS
            || self
                .send_bytes
                .checked_add(send_len)
                .is_none_or(|next| next > MAX_PENDING_LUA_SEND_BYTES)
        {
            return PaneInputResult::Backpressured;
        }
        self.send_bytes += send_len;
        self.commands.push_back(command);
        PaneInputResult::Queued
    }

    fn front(&self) -> Option<&PendingLuaCommand> {
        self.commands.front()
    }

    fn front_mut(&mut self) -> Option<&mut PendingLuaCommand> {
        self.commands.front_mut()
    }

    fn pop_front(&mut self) -> Option<PendingLuaCommand> {
        let command = self.commands.pop_front()?;
        self.send_bytes = self.send_bytes.saturating_sub(command.send_len());
        Some(command)
    }

    fn is_empty(&self) -> bool {
        self.commands.is_empty()
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.commands.len()
    }
}

#[derive(Debug)]
struct AutomationRetry {
    next_at: Option<std::time::Instant>,
    backoff: std::time::Duration,
}

impl Default for AutomationRetry {
    fn default() -> Self {
        Self {
            next_at: None,
            backoff: AUTOMATION_RETRY_MIN,
        }
    }
}

impl AutomationRetry {
    fn is_ready(&self, now: std::time::Instant) -> bool {
        self.next_at.is_none_or(|deadline| now >= deadline)
    }

    fn backpressured(&mut self, now: std::time::Instant) {
        self.next_at = Some(now + self.backoff);
        self.backoff = self.backoff.saturating_mul(2).min(AUTOMATION_RETRY_MAX);
    }

    fn progressed(&mut self) {
        self.next_at = None;
        self.backoff = AUTOMATION_RETRY_MIN;
    }

    fn yield_soon(&mut self, now: std::time::Instant) {
        let deadline = now + AUTOMATION_YIELD;
        if self.next_at.is_none_or(|current| deadline < current) {
            self.next_at = Some(deadline);
        }
    }

    fn expedite_if_idle(&mut self, now: std::time::Instant) {
        if self.next_at.is_none() {
            self.next_at = Some(now);
        }
    }

    fn remaining(&self, now: std::time::Instant) -> Option<std::time::Duration> {
        self.next_at.map(|deadline| {
            deadline
                .saturating_duration_since(now)
                .max(std::time::Duration::from_nanos(1))
        })
    }
}

fn input_rejection_message(result: PaneInputResult) -> Option<&'static str> {
    match result {
        PaneInputResult::Queued | PaneInputResult::ReadOnly => None,
        PaneInputResult::Backpressured => Some(
            "Input could not be delivered because a child is not accepting it fast enough. If broadcast mode is active, some panes may already have received it; verify before retrying.",
        ),
        PaneInputResult::Oversize => Some(
            "Input exceeded Kettle's per-message safety limit and was not delivered. If broadcast mode is active, verify the other panes before retrying.",
        ),
        PaneInputResult::Failed => {
            Some("The pane's input transport failed. Kettle will close the failed pane.")
        }
    }
}

/// The notification for a rejected input, in the UI's language. Logs keep
/// [`input_rejection_message`]'s English.
fn input_rejection_text(
    result: PaneInputResult,
    tr: &kettle_i18n::Translator,
) -> Option<&'static str> {
    use kettle_i18n::Text;
    input_rejection_message(result)?;
    Some(tr.text(match result {
        PaneInputResult::Backpressured => Text::NotifyBodyInputBackpressured,
        PaneInputResult::Oversize => Text::NotifyBodyInputOversize,
        _ => Text::NotifyBodyInputFailed,
    }))
}

fn should_notify_input_rejection(
    last: Option<(PaneInputResult, std::time::Instant)>,
    result: PaneInputResult,
    now: std::time::Instant,
) -> bool {
    input_rejection_message(result).is_some()
        && last.is_none_or(|(prior, at)| {
            prior != result || now.saturating_duration_since(at) >= INPUT_REJECTION_NOTICE_INTERVAL
        })
}

fn ctl_input_error(result: PaneInputResult) -> Option<(&'static str, &'static str)> {
    use kettle_ctl::protocol::error_codes as ec;
    match result {
        PaneInputResult::Queued => None,
        PaneInputResult::ReadOnly => Some((
            ec::READ_ONLY,
            "pane is read-only (user toggled 'Read only')",
        )),
        PaneInputResult::Backpressured => Some((
            ec::BUSY,
            "pane input is backpressured; no bytes were sent (retry later)",
        )),
        PaneInputResult::Oversize => Some((
            ec::BAD_PARAMS,
            "input exceeds the per-message safety limit; no bytes were sent",
        )),
        PaneInputResult::Failed => Some((
            ec::INTERNAL,
            "pane input transport failed; the pane is being closed",
        )),
    }
}

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

/// How much of the saved session can be stale before the next event-loop turn
/// writes it out.
///
/// Most of what `session.json` holds is announced by a gesture that already
/// saves — splitting, closing, opening a tab. Some of it is not: a shell `cd`
/// moves a pane's recorded directory, a divider drag moves a split ratio, a
/// renamed tab moves its title. Without the sweep, each would reach disk only
/// if some *later* gesture happened to save, so restoring a workspace could
/// bring back directories the user left an hour ago.
///
/// Saving on every turn instead would serialize the whole tree at PTY-output
/// rates, and arming a timer for it would keep the process awake and show up as
/// idle CPU — kettle measures that. So the sweep rides on turns the event loop
/// was running anyway: it never wakes the process on its own account, and an
/// idle kettle simply saves the moment anything next happens. The write itself
/// is skipped when the serialized text matches what is already on disk, so a
/// sweep over an unchanged workspace costs one serialization and no I/O.
const SESSION_SWEEP: std::time::Duration = std::time::Duration::from_secs(2);

/// The right-click row that reopens a detected remote session. Users, hosts,
/// runtimes and containers are shown as detected.
fn remote_reconnect_label(
    ctx: &kettle_remote::RemoteContext,
    tr: &kettle_i18n::Translator,
) -> String {
    match ctx {
        kettle_remote::RemoteContext::Ssh { host, user, .. } => match user {
            Some(user) => tr.menu_reconnect_ssh_user(user, host),
            None => tr.menu_reconnect_ssh(host),
        },
        kettle_remote::RemoteContext::Container {
            runtime, container, ..
        } => tr.menu_reattach(runtime.name(), container),
    }
}

fn remote_reconnect_item(
    ctx: &kettle_remote::RemoteContext,
    tr: &kettle_i18n::Translator,
) -> Option<ContextMenuItem> {
    Some(ContextMenuItem::ConfigItem {
        label: remote_reconnect_label(ctx, tr),
        command: kettle_remote::clone_session_command(ctx)?,
        new_pane: true,
    })
}

fn reconnect_remote_session(
    mux: &mut Mux,
    cfg: &Config,
    geometry: PtyGeometry,
    waker: kettle_core::Waker,
    mut command: String,
) -> Result<PaneInputResult> {
    mux.split_with_geometry(
        Dir::Horizontal,
        false,
        cfg,
        geometry,
        waker,
        cfg.shell.iter().cloned().collect(),
        None,
    )?;
    let pane = mux
        .focused()
        .ok_or_else(|| anyhow::anyhow!("remote reconnect created no pane"))?;
    command.push('\n');
    Ok(pane.feed_input(command.as_bytes()))
}

/// Which broadcast scope the group chord turns on, from `broadcast-default`.
///
/// Terminator stores this key as the *initial* groupsend mode
/// (`terminator.py`: `groupsend = groupsend_type[config['broadcast_default']]`).
/// kettle reads it as the scope the chord selects instead, because a window
/// that started in `all` would mirror every keystroke into every pane before
/// the user had touched anything.
///
/// For Terminator's own default the two readings are the same thing — its
/// `group` mode with no groups yet assigned sends only to the focused terminal
/// — so the divergence is visible only under `broadcast-default = all`, where
/// kettle waits to be asked. `group` maps to kettle's per-tab broadcast, which
/// is what a kettle "group" has always meant to the chord.
fn broadcast_scope_for_default(
    default: kettle_config::BroadcastDefault,
) -> crate::mux::BroadcastScope {
    match default {
        kettle_config::BroadcastDefault::All => crate::mux::BroadcastScope::All,
        kettle_config::BroadcastDefault::Group => crate::mux::BroadcastScope::Tab,
        kettle_config::BroadcastDefault::Off => crate::mux::BroadcastScope::Off,
    }
}

/// Whether a session sweep is owed as of `now`. The first turn always sweeps,
/// so a workspace that changes and then goes quiet is written once rather than
/// waiting for a second turn that may never come.
///
/// A window with no tabs is never swept, whatever the clock says. A sweep is a
/// background refresh of what is on screen, and an empty window is one that has
/// not opened yet or is on its way out — writing that snapshot would put
/// `{"tabs":[],"windows":[]}` over the saved session the user is about to have
/// restored. The paths that genuinely mean to erase — `close_window`, and the
/// reap that precedes this — still save explicitly, and this does not touch
/// them. Same rule as the named-layout guard in `session_write_target`.
///
/// Returning `false` here deliberately leaves the caller's timestamp alone, so
/// a window that is skipped does not consume the interval for the others.
fn session_sweep_due(
    last: Option<std::time::Instant>,
    now: std::time::Instant,
    window_has_tabs: bool,
) -> bool {
    window_has_tabs && last.is_none_or(|then| now.saturating_duration_since(then) >= SESSION_SWEEP)
}

/// Rescue-tick cadence (one frame at 60Hz) while a torn drag is in flight or
/// a silent native handoff is being watched.
const TORN_TICK_MS: u64 = 16;

/// Diagnostics/test override: `KETTLE_TEAR_MANUAL_FOLLOW=1` skips the native
/// `drag_window()` handoff so a tear always takes the manual-follow/rescue-tick
/// path. Otherwise demotion is a nondeterministic race (the WM silently
/// dropping `_NET_WM_MOVERESIZE` for a just-mapped window) that cannot be
/// triggered on demand, so the live tear-off smoke pins BOTH paths by
/// launching one instance per mode. Read per tear, not cached: trivially
/// cheap, and it keeps the override in one place.
fn tear_native_handoff_disabled() -> bool {
    std::env::var_os("KETTLE_TEAR_MANUAL_FOLLOW").is_some_and(|v| v == "1")
}

/// Real pointer travel (px) required, alongside the 400ms grace, before a
/// silent native handoff is demoted to manual follow. A WM that took the grab
/// moves the frame with the pointer, so travel-without-`Moved` is proof it
/// never did; the floor keeps a stationary hover from demoting a
/// healthy-but-quiet grab.
const DEMOTION_TRAVEL_PX: f64 = 24.0;

#[cfg(any(target_os = "linux", test))]
#[derive(PartialEq, Eq)]
struct WaylandDropTarget {
    pane: Option<u64>,
    broadcast: crate::mux::BroadcastScope,
    input_generation: u64,
    preview: Option<crate::media::PreviewFocus>,
    pane_input: Option<u64>,
}

#[cfg(any(target_os = "linux", test))]
impl WaylandDropTarget {
    fn capture(ws: &WindowState) -> Self {
        Self {
            pane: ws.mux.active_focus(),
            broadcast: ws.mux.broadcast.clone(),
            input_generation: ws.ime_focus_generation,
            preview: ws.preview_focus,
            pane_input: ws
                .mux
                .active_focus()
                .and_then(|id| ws.mux.panes.get(&id))
                .map(|pane| pane.input_generation()),
        }
    }
}

#[cfg(target_os = "linux")]
struct WaylandTabDrag {
    source: u64,
    source_index: usize,
    panes: Vec<u64>,
    carrier: u64,
    preview: Option<u64>,
}

struct TornDrag {
    /// Seq of the torn-off window being dragged.
    seq: u64,
    /// Seq of the mouse-capture holder: the tear's SOURCE window (it keeps
    /// streaming CursorMoved while the button is held), or `seq` itself
    /// for a lone-tab whole-window drag. Manual-follow only listens to the
    /// carrier — without this gate, stale tracking would hijack EVERY
    /// window's cursor stream.
    carrier: u64,
    /// Handoff instant. The X11/macOS pointer-event drop heuristics are
    /// suppressed for a short window after it (a stray client motion can
    /// race the WM actually taking the move grab), and the native→manual
    /// demotion fires off it when the WM never takes the drag at all.
    started: std::time::Instant,
    /// Cursor offset from the torn window's FRAME top-left in drag coordinates
    /// (macOS desktop points, physical pixels on Windows/X11),
    /// chosen at tear time so the pointer holds the tab. `Moved(pos) + grab`
    /// approximates the live screen cursor during the native move loop
    /// (winit's `Moved` reports the frame position on Windows — verified in
    /// the vendored 0.30.13 WM_WINDOWPOSCHANGED handler).
    grab: (f64, f64),
    /// Latched dock target: (sibling window seq, insertion index). Updated
    /// by every hit-test; the drop commits whatever is latched.
    dock: Option<(u64, usize)>,
    /// `drag_window()` succeeded — the OS carries the window. The drop then
    /// arrives as winit's synthesized post-WM_EXITSIZEMOVE left-release
    /// (Windows) or as the first client pointer event after the WM's pointer
    /// grab ends (X11/macOS — the client receives NO pointer events while
    /// the WM moves the window). `false` = manual-follow: the SOURCE window
    /// still holds mouse capture, repositions the torn window from its own
    /// CursorMoved stream, and its left-release is the drop.
    native: bool,
    /// A native caption gesture is OS-owned and must never be demoted to
    /// the manual-follow rescue used for failed tab-drag handoffs.
    caption: bool,
    /// At least one `Moved` arrived since the handoff. Gates the
    /// pointer-event drop heuristic against the PostMessage race on
    /// Windows (a stray client mouse-move can slip in between
    /// `drag_window()`'s posted WM_NCLBUTTONDOWN and the modal loop
    /// actually starting) and against an X11 tear the user never moved.
    saw_move: bool,
    /// Last signal (tear or `Moved`) — the stale-tracking failsafe: a
    /// torn drag with no movement for 120s is abandoned by `about_to_wait`
    /// (covers an X11 tear whose release we never observe). The rescue
    /// tick also arbitrates on it (a fresh CursorMoved-driven follow
    /// suppresses the tick for 30ms).
    last_signal: std::time::Instant,
    /// The torn window's HWND (Windows; `None` elsewhere) — the dock-hover
    /// translucency and the z-order walk both key off it, and tracking
    /// teardown needs it to restore full opacity from any code path.
    /// Only Windows code reads it; populated unconditionally so the
    /// struct shape stays identical across platforms.
    #[cfg_attr(not(target_os = "windows"), allow(dead_code))]
    hwnd: Option<isize>,
    /// X11: the real cursor position at the last tracking signal, refreshed
    /// by every `Moved` that pairs with a live-cursor query, or by the rescue
    /// tick's first look. Demotion evidence: if the pointer has since
    /// travelled while no further `Moved` arrived, the WM is not actually
    /// moving the window (an accepted-but-ignored `_NET_WM_MOVERESIZE`, an
    /// unmapped-window race, or a grab dropped after one incidental placement
    /// `Moved`) and the tick takes over. Purely time-based demotion would
    /// fight a WM that DID take the grab over a pointer simply holding still.
    signal_cursor: Option<(f64, f64)>,
}

/// Queue a desktop notification without ever waiting on an OS notification
/// service from Kettle's UI thread. The single bounded worker preserves order;
/// a stalled backend fills the queue and drops later notifications instead of
/// freezing rendering, input, and control replies.
pub(crate) fn fire_notify(title: &str, body: &str) {
    crate::queue_desktop_notification(title, body);
}

/// A split that cannot spawn leaves the layout exactly as it was, so all the
/// user sees is a keystroke that did nothing. That is indistinguishable from a
/// split that appeared and never drew, and a `warn!` line is invisible at the
/// default log level, so the two reports arrive looking identical. Say it out
/// loud instead, the way a failed preference write already does.
pub(crate) fn report_split_failure(
    direction: &str,
    error: &dyn std::fmt::Display,
    tr: &kettle_i18n::Translator,
) {
    log::error!("could not split pane ({direction}): {error}");
    fire_notify(
        tr.text(kettle_i18n::Text::NotifyTitleSplitFailed),
        &error.to_string(),
    );
}

fn persist_keybind_rebind(
    path: Option<&std::path::Path>,
    stale: &[String],
    label: &str,
    action_name: &str,
) -> bool {
    let Some(path) = path else {
        log::warn!(
            "apply_keybind_rebind: no config path resolved \
             (set $XDG_CONFIG_HOME or pass --config)"
        );
        return false;
    };

    match kettle_config::rebind_keybind(path, stale, label, action_name) {
        Ok(_) => true,
        Err(e) => {
            log::warn!("rebind_keybind({label}={action_name}) failed: {e}");
            false
        }
    }
}

fn completion_surface_available(mode: kettle_core::TermMode, display_offset: usize) -> bool {
    !mode.contains(kettle_core::TermMode::ALT_SCREEN) && display_offset == 0
}

fn completion_anchor_col(cursor_col: usize, input_prefix: &str) -> usize {
    cursor_col.saturating_sub(unicode_width::UnicodeWidthStr::width(input_prefix))
}

#[cfg(test)]
mod completion_surface_tests;

impl App {
    /// The map key of the WindowState whose OS window is `wid`, if any.
    /// Returns `WindowState::seq`, which is the map key by invariant.
    fn seq_of_window(&self, wid: WindowId) -> Option<u64> {
        self.windows
            .values()
            .find_map(|w| (w.window.as_ref().map(|x| x.id()) == Some(wid)).then_some(w.seq))
    }

    fn request_window_close(&mut self, seq: u64) {
        self.pending_window_closes.insert(seq);
    }

    fn window_close_pending(&self, seq: u64) -> bool {
        self.pending_window_closes.contains(&seq)
    }

    /// The dispatch wrappers' epilogue. Reinserts the checked-out window
    /// — or, when the inner handler flagged this window for close
    /// or a quit, drops it (panes' PTYs die with their Mux) and exits the
    /// event loop once no windows remain. This is the ONLY place a window
    /// close reaches `event_loop.exit()`, so "exit only when the map is
    /// empty" holds by construction (drift-guarded in
    /// `event_loop_exit_sites_are_allowlisted`).
    fn finish_window_dispatch(&mut self, event_loop: &ActiveEventLoop, seq: u64, ws: WindowState) {
        if self.quit_requested {
            if let Some(owner) = self.update_toast.as_ref().map(|toast| toast.window) {
                self.close_update_toast_window(owner);
            }
            drop(ws);
            self.windows.clear();
            event_loop.exit();
            return;
        }
        if take_window_close_request(&mut self.pending_window_closes, seq) {
            self.close_update_toast_window(seq);
            // A dying window that is the torn window or the manual-follow
            // capture holder takes its drag with it. Abandon eagerly (clears
            // the latched preview on the still-mapped target and restores
            // opacity) instead of leaving stale tracking for the heuristics to
            // trip over.
            if self
                .torn_drag
                .as_ref()
                .is_some_and(|t| t.seq == seq || t.carrier == seq)
            {
                self.abandon_torn_drag(None);
            }
            drop(ws);
            self.hoover_mapped_groups();
            if self.windows.is_empty() {
                event_loop.exit();
            } else if self.focused_seq == seq
                && let Some(&next) = self.windows.keys().next_back()
            {
                // Focus falls to the most recently opened remaining window;
                // the OS will confirm with a Focused(true) shortly.
                self.focused_seq = next;
            }
            return;
        }
        let mut ws = ws;
        self.sync_pane_focus_reports(&mut ws);
        self.windows.insert(seq, ws);
    }

    /// Keep focus-event reports (DEC mode 1004) in step with the pane that
    /// holds the keyboard: the focused pane of the active tab while the window
    /// has OS focus. A pane that enabled the mode hears `CSI O` when that
    /// stops being it, whether the window lost focus or focus moved to
    /// another pane or tab (a key, a click, a control request or a close),
    /// and `CSI I` when it becomes it again. tmux `focus-events`, Neovim's
    /// `FocusGained`/`FocusLost` and the agent CLIs' notification state all
    /// rely on that. Every dispatch passes through `finish_window_dispatch`,
    /// so one check there covers every route, and a pane hears each change
    /// once.
    fn sync_pane_focus_reports(&mut self, ws: &mut WindowState) {
        let focused = (ws.os_focus_known && ws.window_focused)
            .then(|| ws.mux.active_focus())
            .flatten();
        let (lost, gained) = focus_report_transition(ws.focus_reported_pane, focused);
        ws.focus_reported_pane = focused;
        for (pane_id, focus) in [(lost, false), (gained, true)] {
            let Some(pane_id) = pane_id else {
                continue;
            };
            if !self
                .pane_mode(ws, pane_id)
                .is_some_and(|mode| mode.contains(kettle_core::TermMode::FOCUS_IN_OUT))
            {
                continue;
            }
            if let Some(pane) = ws.mux.panes.get(&pane_id) {
                let result = pane.feed_focus_report(focus);
                if !result.is_queued() {
                    self.report_input_result(result);
                }
            }
        }
    }

    /// A tab leaving this window takes its panes with it. If one of them holds
    /// the reported focus, tell it focus left now, through the detached tab,
    /// so the window that adopts it starts from "not focused" and its first
    /// `CSI I` follows a `CSI O`. Called only after a detach succeeded, so a
    /// failed transfer reports nothing.
    fn release_focus_report_to(&mut self, ws: &mut WindowState, dt: &crate::mux::DetachedTab) {
        let Some(reported) = ws.focus_reported_pane else {
            return;
        };
        let Some((_, pane)) = dt.panes.iter().find(|(id, _)| *id == reported) else {
            return;
        };
        ws.focus_reported_pane = None;
        let reporting = pane
            .term
            .term
            .lock()
            .ok()
            .is_some_and(|t| t.mode().contains(kettle_core::TermMode::FOCUS_IN_OUT));
        if reporting {
            let result = pane.feed_focus_report(false);
            if !result.is_queued() {
                self.report_input_result(result);
            }
        }
    }

    fn focus_or_open_window(&mut self, event_loop: &ActiveEventLoop, cwd: Option<String>) -> bool {
        let target = self
            .windows
            .get(&self.focused_seq)
            .or_else(|| self.windows.values().max_by_key(|window| window.seq));
        if let Some(ws) = target
            && let Some(window) = &ws.window
        {
            self.focused_seq = ws.seq;
            window.set_visible(true);
            window.set_minimized(false);
            window.focus_window();
            return true;
        }
        self.open_window(event_loop, WindowOpen::Fresh { cwd }, None, None)
            .is_ok()
    }

    fn drain_activation_requests(&mut self, event_loop: &ActiveEventLoop) {
        // Keep requests queued while window 1 is still creating the shared GPU
        // context. `resumed` calls this again immediately after initialization.
        if self.gpu.is_none() {
            return;
        }
        let pending = self
            .activation
            .as_ref()
            .map(crate::activation_server::ActivationInbox::drain)
            .unwrap_or_default();
        for request in pending {
            if !request.claim() {
                continue;
            }
            if !activation_recording_available(
                request.request.requires_recording(),
                self.recording_status(),
            ) {
                log::warn!(
                    "bare-launch activation rejected because the requested dev recorder is not active"
                );
                let _ = request.completion.send(false);
                continue;
            }
            let cwd = request.request.cwd().map(str::to_string);
            let opened = if request.request.focuses_existing() {
                self.focus_or_open_window(event_loop, cwd)
            } else {
                self.open_window(event_loop, WindowOpen::Fresh { cwd }, None, None)
                    .is_ok()
            };
            if opened {
                log::info!("bare-launch activation activated a window");
                if !request.request.focuses_existing()
                    && let Some(recorder) = self.recorder.as_mut()
                {
                    recorder.record_marker("kettle:activation_window");
                }
            }
            let _ = request.completion.send(opened);
        }
    }

    pub fn run() -> Result<()> {
        Self::run_with(crate::Options::default())
    }

    pub fn run_with(mut startup: crate::Options) -> Result<()> {
        crate::startup_trace::mark(crate::startup_trace::Phase::RunWith);
        let _trace = crate::startup_trace::FlushOnDrop;
        // Enumerate the system fonts while the event loop starts (AppKit's
        // launch, on macOS) instead of after it.
        let mut font_preload = crate::font_preload::FontPreload::start();
        // Reclaim pasted-image directories from a run that died before its own
        // cleanup. Age-gated, so a sibling instance mid-session is untouched.
        crate::paste_image::sweep_stale();
        #[cfg(target_os = "macos")]
        disable_app_state_restoration();
        let event_loop = EventLoop::<UserEvent>::with_user_event().build()?;
        crate::startup_trace::mark(crate::startup_trace::Phase::EventLoopBuilt);
        event_loop.set_control_flow(ControlFlow::Wait);
        let proxy = event_loop.create_proxy();
        // Primaries of other builds, such as the old version's after an
        // update: shared state they own waits until they exit.
        let other_builds = startup
            .activation
            .as_ref()
            .map(kettle_ctl::activation::PrimaryHandle::other_builds)
            .unwrap_or_default();
        let another_build_running = other_builds.running();
        let activation = startup.activation.take().and_then(|primary| {
            match crate::activation_server::ActivationInbox::start(primary, proxy.clone()) {
                Ok(inbox) => Some(inbox),
                Err(error) => {
                    log::warn!("bare-launch activation server unavailable: {error}");
                    None
                }
            }
        });

        // Watch the chosen config file's directory for live reload, but reload
        // only on events whose `paths` contain the config file itself. Other
        // files there change often (session.json saves, palette edits, the
        // theme cache, editor swap files), and each atomic `save_session`
        // alone writes `session.json.tmp.*` then renames it, firing 3+ events.
        let config_reload_pending = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watcher = startup
            .config
            .clone()
            .or_else(Config::default_path)
            .and_then(|path| {
                let p = proxy.clone();
                config_watcher(
                    &path,
                    startup.config_trust,
                    config_reload_pending.clone(),
                    move || p.send_event(UserEvent::ReloadConfig).is_ok(),
                )
                .map_err(|error| {
                    log::warn!(
                        "config live-reload unavailable for {}: {error}",
                        path.display()
                    );
                })
                .ok()
            });

        // Remote-control watcher. Same notify pattern as the
        // config-reload watcher above. When startup.remote_file is
        // Some, watch its parent directory; on a change to the file
        // itself, send UserEvent::RemoteCommand so the main thread
        // reads + dispatches lines.
        let mut remote_watcher = None;
        let mut remote_startup_retry = false;
        let remote_command_pending = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        if let Some(path) = startup.remote_file.clone()
            && let Some(dir) = path.parent().map(|p| p.to_path_buf())
        {
            let p = proxy.clone();
            let watched = path.clone();
            let pending = remote_command_pending.clone();
            // `remote.cmd` is a command channel into this terminal, so its
            // parent must be private before the lock below touches anything.
            // Repairing here rather than relying on some other subsystem
            // having run first is the difference between the watcher working
            // on this launch and on the next one.
            let _ = kettle_state::create_private_dirs(&dir);
            use notify::Watcher;
            let built = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res
                    && watcher_event_matches(&ev, &watched)
                {
                    queue_watcher_event(&pending, || {
                        p.send_event(UserEvent::RemoteCommand).is_ok()
                    });
                }
            });
            // Same silent constructor as the config watcher above, and the same
            // reason to report it: this one carries remote commands, so a
            // channel that never subscribes is worth a line in the log.
            if let Err(error) = &built {
                log::warn!("remote-command watcher unavailable: {error}");
            }
            if let Ok(mut w) = built {
                // Create or harden the control file before registering its
                // watcher, and truncate through that verified handle so a
                // reparse/path swap cannot redirect startup cleanup. Commands
                // left after a previous crash must not replay into this run.
                let lock_path = kettle_state::remote_command_lock_path(&path);
                match kettle_state::ExclusiveFileLock::try_acquire(&lock_path) {
                    Ok(Some(_lock)) => {
                        // Keep the sender/receiver lock through watcher
                        // registration. Once it is released, every later
                        // append is observed; there is no truncate→watch gap
                        // in which a valid command can be stranded.
                        let initialized = initialize_remote_spool(&path, another_build_running)
                            .and_then(|()| {
                                w.watch(&dir, notify::RecursiveMode::NonRecursive)
                                    .map_err(std::io::Error::other)
                            });
                        match initialized {
                            Ok(()) => remote_watcher = Some(w),
                            Err(error) => log::warn!(
                                "remote-command watcher disabled for {}: {error}",
                                path.display()
                            ),
                        }
                    }
                    Ok(None) => {
                        // A live sender owns the spool. Register the watcher
                        // immediately and claim asynchronously once it releases
                        // the lock; waiting and then truncating would erase the
                        // command that sender just finished appending.
                        match w
                            .watch(&dir, notify::RecursiveMode::NonRecursive)
                            .map_err(std::io::Error::other)
                        {
                            Ok(()) => {
                                log::warn!(
                                    "remote-command startup lock at {} is busy; \
                                     watcher is active and the spool will be claimed asynchronously",
                                    lock_path.display()
                                );
                                remote_startup_retry = true;
                                remote_watcher = Some(w);
                            }
                            Err(watch_error) => log::warn!(
                                "remote-command watcher disabled for {} while its startup lock \
                                 was busy: {watch_error}",
                                path.display()
                            ),
                        }
                    }
                    Err(error) => log::warn!(
                        "remote-command watcher disabled for {}: {error}",
                        path.display()
                    ),
                }
            }
        }

        // Load the config into a local so the trigger compile and the `cfg`
        // field below can both use it; inside the struct init, a bare `cfg.…`
        // would hit the `cfg!()` macro instead.
        let initial_path = startup.config.clone().or_else(Config::default_path);
        let mut initial_cfg = match initial_path.as_deref().filter(|path| path.exists()) {
            Some(path) => match prepare_config_directory(path, startup.config_trust)
                .and_then(|()| Config::read_from_with_trust(path, startup.config_trust))
            {
                Ok(loaded) => {
                    if !loaded.unknown_keys.is_empty() {
                        log::warn!(
                            "{}: unrecognized config keys: {}",
                            path.display(),
                            loaded.unknown_keys.join(", ")
                        );
                    }
                    if !loaded.malformed_values.is_empty() {
                        log::warn!(
                            "{}: malformed values (ignored): {}",
                            path.display(),
                            loaded.malformed_values.join(", ")
                        );
                    }
                    loaded.config
                }
                Err(error) => {
                    log::warn!("config {} ignored: {error}", path.display());
                    // No `language` to read, so the notice follows the OS.
                    let tr = kettle_i18n::Translator::new(
                        kettle_config::LanguagePreference::Auto.resolve(kettle_i18n::os_locale),
                    );
                    fire_notify(
                        tr.text(kettle_i18n::Text::NotifyTitleConfigIgnored),
                        &tr.notify_body_config_ignored(
                            &path.display().to_string(),
                            &error.to_string(),
                        ),
                    );
                    Config::default()
                }
            },
            None => Config::default(),
        };
        // The process's UI language, fixed for its lifetime: a config reload
        // keeps it, and Settings says a change applies on restart.
        let ui_text =
            kettle_i18n::Translator::new(initial_cfg.language.resolve(kettle_i18n::os_locale));
        // `EventLoop::build` registers and installs winit's application
        // delegate, so the Dock menu can only be grafted on after it; its rows
        // need the UI language, so after the config is read too.
        crate::macos_dock::install(proxy.clone(), &ui_text);
        // Peacock parity: the --accent CLI flag wins over the config
        // `accent-color` key. Applied here at startup and again on every
        // config reload, since CLI flags are launch-time intent.
        if let Some(rgb) = startup.accent_override {
            initial_cfg.accent_color = Some(rgb);
        }
        // Terminator parity: launch-time window-state CLI flags
        // (`-m/-f/-H/-b/-T`) override the config for THIS launch — same
        // "CLI flags are launch-time intent" precedent as --accent above.
        if let Some(wstate) = startup.window_state_override {
            initial_cfg.window_state = wstate;
        }
        if let Some(b) = startup.borderless_override {
            initial_cfg.borderless = b;
        }
        if let Some(title) = startup.title_override.clone() {
            // A literal title (no `{title}` placeholder) renders verbatim.
            initial_cfg.window_title_format = title;
        }
        // Peacock: seed the per-window accent variation from this
        // window's working directory, so `accent-color = auto` gives a window
        // in a different project a different (but per-project stable) accent.
        initial_cfg.accent_seed = accent_seed_from_cwd(startup.cwd.as_deref());
        crate::startup_trace::mark(crate::startup_trace::Phase::ConfigLoaded);
        font_preload.request_family(&initial_cfg.font_family);
        let initial_triggers = compile_triggers(&initial_cfg.triggers);
        // Dropdown-parity: capture before `startup` moves into the App.
        let startup_version = startup
            .version
            .clone()
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").to_string());
        let runtime_tracker = crate::runtime_diagnostics::RuntimeTracker::start(
            cache_dir_from_env(|key| std::env::var(key).ok()),
            startup_version.clone(),
        );
        // Lua scripting foundation. If `--lua-script PATH`
        // was set, init a LuaEngine + run the script once. Failures
        // log::warn but don't block the launch (same shape as
        // `compile_triggers`'s per-pattern warn-and-skip fallthrough).
        //
        // Also drain pending side-effect commands queued
        // by Lua and stash them on App so the first focused pane
        // gets them written to its PTY once it's ready (the pane
        // doesn't exist yet at this point in `run_with`).
        let mut pending_lua_commands = PendingLuaCommands::default();
        let mut initial_lua_queue_rejection_reported = false;
        // Keep the LuaEngine alive on App so kettle.on(...) registrations
        // survive startup and can be fire_event'd from emission sites. With no
        // --lua-script and no `<config-dir>/init.lua`, engine init is skipped
        // so non-Lua runs stay zero-cost.
        //
        // Script resolution:
        //   1. --lua-script PATH (explicit; overrides)
        //   2. <config-dir>/init.lua  (auto-loaded; default for plugins)
        // where <config-dir> is the parent of Config::default_path().
        let init_lua = resolved_init_lua_script(
            startup.lua_script.clone(),
            kettle_config::Config::default_path(),
        );
        let mut lua_engine: Option<crate::LuaEngine> = None;
        if let Some((script, trust)) = &init_lua {
            match crate::LuaEngine::new_with_config_path(
                &initial_cfg.theme_name,
                initial_cfg.lua_sandbox,
                initial_path.as_deref(),
            ) {
                Ok(eng) => {
                    let executed = match trust {
                        ConfigTrust::ExplicitPath => eng.exec_file(script),
                        ConfigTrust::VerifyDirectory => eng.exec_trusted_file(script),
                    };
                    if let Err(e) = executed {
                        log::warn!("lua script {}: {e:#}", script.display());
                    } else {
                        log::info!("lua script {}: executed", script.display());
                    }
                    for cmd in eng.drain_commands() {
                        let result = match cmd {
                            crate::LuaCommand::SendText(s) => {
                                pending_lua_commands.push(PendingLuaCommand::SendText {
                                    origin_window: 1,
                                    target_pane: None,
                                    bytes: Arc::from(s.into_bytes()),
                                })
                            }
                            crate::LuaCommand::ExecAction(name) => {
                                if let Some(a) = kettle_config::Action::from_name(&name) {
                                    pending_lua_commands.push(PendingLuaCommand::ExecAction {
                                        origin_window: 1,
                                        action: a,
                                    })
                                } else {
                                    log::warn!(
                                        "lua kettle.exec_action: unknown action name {name:?}"
                                    );
                                    PaneInputResult::Queued
                                }
                            }
                            crate::LuaCommand::Notify { title, body } => {
                                pending_lua_commands.push(PendingLuaCommand::Notify {
                                    origin_window: 1,
                                    title,
                                    body,
                                })
                            }
                            crate::LuaCommand::CallbackStopped { event, dropped } => {
                                pending_lua_commands.push(PendingLuaCommand::CallbackStopped {
                                    origin_window: 1,
                                    event,
                                    dropped,
                                })
                            }
                            crate::LuaCommand::SetTheme(name) => {
                                pending_lua_commands.push(PendingLuaCommand::SetTheme {
                                    origin_window: 1,
                                    name,
                                })
                            }
                        };
                        if result != PaneInputResult::Queued
                            && !initial_lua_queue_rejection_reported
                            && let Some(message) = input_rejection_message(result)
                        {
                            log::warn!("lua startup command rejected ({result:?}): {message}");
                            fire_notify(
                                ui_text.text(kettle_i18n::Text::NotifyTitleLuaNotQueued),
                                input_rejection_text(result, &ui_text).unwrap_or(message),
                            );
                            initial_lua_queue_rejection_reported = true;
                        }
                    }
                    lua_engine = Some(eng);
                }
                Err(e) => {
                    log::warn!("lua engine init failed: {e:#}");
                }
            }
        }
        // If a LuaEngine is active,
        // the Mux must subscribe to per-PTY output bytes so the
        // App can fire LuaEvent::Output. Set before the Mux moves
        // into the struct.
        let lua_output_subscribed = lua_engine.is_some();
        // Also subscribe to per-pane PTY output when recording is requested for
        // this launch — either the one-shot CLI/env target (`startup.record`) OR
        // the persistent `record = on` config key (`initial_cfg.record`) — so the
        // recorder can tee output into the asciicast trace. The first pane is
        // spawned (with this flag baked in) BEFORE the recorder starts in
        // `resumed_inner`, so a missing config check here would leave the `o`
        // (terminal output) channel permanently empty while still showing `[REC]`.
        let recording_requested = startup.record.is_some() || initial_cfg.record.enabled();
        // Retention budgets are process-wide in kettle-core, so publish them
        // before any recorder can be constructed.
        crate::dev_record::apply_retention_config(&initial_cfg);
        let lua_output_subscribed = lua_output_subscribed || recording_requested;
        // Broadcast always starts off; `broadcast-default` only picks the
        // scope the toggle chord turns on (`broadcast_scope_for_default`).
        // Resolve clipboard availability before constructing the first Mux:
        // DA1 extension 52 is a runtime capability, not merely compiled code,
        // and must not be advertised when policy or the platform denies writes.
        let clipboard = match arboard::Clipboard::new() {
            Ok(cb) => Some(cb),
            Err(e) => {
                log::warn!(
                    "clipboard unavailable ({e}); copy/paste and OSC 52 \
                     will no-op — no DISPLAY/Wayland, headless SSH, or a \
                     sandbox without clipboard-portal access?"
                );
                None
            }
        };
        let osc52_copy_allowed = osc52_copy_is_available(initial_cfg.osc52, clipboard.is_some());

        // Per-window state lives in a WindowState; the first window is seq 1.
        // The Mux is built here because its construction flags
        // (`lua_output_subscribed`, `record_lossless`) are process-global
        // decisions.
        let mux = {
            let mut m = Mux::new();
            m.lua_output_subscribed = lua_output_subscribed;
            // A recording needs a lossless output channel so the asciicast
            // trace can't drop chunks under a fast burst (CLI/env target or the
            // `record = on` config key — see `recording_requested` above).
            {
                m.record_lossless = recording_requested;
            }
            m.osc52_copy_allowed = osc52_copy_allowed;
            m.autoclean_groups = initial_cfg.autoclean_groups;
            m
        };
        let mut windows = std::collections::BTreeMap::new();
        let mut initial_window = WindowState::new(1, mux);
        initial_window.cursor_exit_log = crate::cursor_exit_log::CursorExitLog::from_env(1);
        windows.insert(1, initial_window);
        let remote_proxy = proxy.clone();
        let remote_scan_worker =
            match kettle_remote::RemoteScanWorker::spawn_with_notifier(move || {
                let _ = remote_proxy.send_event(UserEvent::RemoteScanReady);
            }) {
                Ok(worker) => Some(worker),
                Err(error) => {
                    log::warn!(
                        "remote-context detection disabled: cannot start scanner worker: {error}"
                    );
                    None
                }
            };
        let video_previewer =
            crate::video_preview::VideoPreviewer::new(proxy.clone(), startup.media.clone());
        let plan = startup_plan(&startup, &initial_cfg, another_build_running);
        let ctl_policy = ctl_overrides(&startup).resolve(initial_cfg.ctl_policy());
        let mut app = App {
            #[cfg(target_os = "linux")]
            wayland: None,
            #[cfg(target_os = "linux")]
            wayland_tab_drag: None,
            #[cfg(target_os = "linux")]
            wayland_file_drops: std::collections::BTreeMap::new(),
            restore_source: None,
            session_writer: crate::session::SessionWriter::new()?,
            cfg: initial_cfg,
            // Assume writable; `resumed` clears it when a launch override
            // means the named layout was never loaded.
            named_layout_writable: true,
            windows,
            accent_registry: crate::window_accent::AccentRegistry::default(),
            focused_seq: 1,
            next_window_seq: 2,
            gpu: None,
            font_preload: Some(font_preload),
            launch_override: plan.launch_override,
            pre_launch_eligible: plan.pre_launch && !plan.may_restore,
            gpu_choices: Vec::new(),
            ui_text,
            gpu_recovery: RecoveryState::default(),
            gpu_incident: None,
            gpu_incident_started_for_loss: false,
            gpu_software_fallback: false,
            window_icon_dark_synced: None,
            pending_window_closes: std::collections::BTreeSet::new(),
            quit_requested: false,
            recorder: None,
            recording_start_failed: false,
            recording_error_reported: false,
            last_input_rejection_notice: None,
            system_motion: std::sync::OnceLock::new(),
            #[cfg(target_os = "linux")]
            agent_launches: kettle_ctl::agent_launch::LaunchScopes::default(),
            proxy,
            activation,
            runtime_tracker,
            wake_pending: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            config_reload_pending,
            config_reload_deadline: None,
            remote_command_pending,
            pending_remote_commands: VecDeque::new(),
            remote_spool_claim_pending: remote_startup_retry,
            remote_command_retry: {
                let mut retry = AutomationRetry::default();
                if remote_startup_retry {
                    retry.expedite_if_idle(std::time::Instant::now());
                }
                retry
            },
            // Server is started later in `resumed`
            // (needs the pid + a live event-loop proxy for the waker).
            ctl: None,
            ctl_policy,
            ctl_start: CtlStart::NotReady,
            pending_runs: std::collections::HashMap::new(),
            media: crate::media::MediaService::default(),
            cards_tip: crate::media::CardsTip::load(),
            clipboard,
            pasted_images: crate::paste_image::PastedImages::new(crate::paste_image::PASTED),
            png_encoder: None,
            png_jobs: 0,
            queued_pastes: std::collections::VecDeque::new(),
            viewer_copies: std::sync::Arc::new(std::sync::Mutex::new(
                crate::paste_image::PastedImages::new(crate::paste_image::OPENED),
            )),
            video_copies: std::sync::Arc::new(crate::media::PrivateCopies::new(
                crate::paste_image::VIDEOS,
            )),
            document_copies: std::sync::Arc::new(crate::media::PrivateCopies::new(
                crate::paste_image::DOCUMENTS,
            )),
            previews_ready: Vec::new(),
            ctl_driving: false,
            lua_driving: false,
            crop_tickets: 0,
            page_tickets: 0,
            preview_tickets: 0,
            preview_copy: None,
            video_previewer,
            next_video_preview_generation: 1,
            compiled_triggers: initial_triggers,
            last_trigger_fire: None,
            remote_scan_worker,
            remote_applied: std::collections::HashMap::new(),
            remote_live_panes_scratch: std::collections::HashSet::new(),
            remote_targets_scratch: Vec::new(),
            last_remote_poll: None,
            remote_poll_due: None,
            last_schedule_decision: None,
            os_theme_dark: None,
            config_path: startup.config.clone(),
            config_trust: startup.config_trust,
            config_malformed_last: Vec::new(),
            config_watcher_path: watcher
                .as_ref()
                .and_then(|_| startup.config.clone().or_else(Config::default_path)),
            startup,
            _watcher: watcher,
            _remote_watcher: remote_watcher,
            pending_lua_commands,
            lua_command_retry: AutomationRetry::default(),
            lua_engine,
            lua_startup_fired: false,
            update_facts: crate::update_notice::UpdateFacts::default(),
            update_local_dismissals: crate::update_notice::LocalDismissals::default(),
            update_restart_dismissed: false,
            update_toast: None,
            update_toast_receipts: None,
            update_toast_claiming: None,
            update_toast_handled: None,
            update_toast_unavailable: None,
            update_toast_retry: None,
            update_toast_focus: None,
            update_facts_requested: None,
            update_facts_refresh: Default::default(),
            logged_restart_pending: None,
            other_builds,
            executable: std::env::current_exe().and_then(std::fs::canonicalize).ok(),
            version_line: startup_version,
            torn_drag: None,
            last_session_sweep: None,
        };
        app.runtime_tracker.set_window_count(app.windows.len());
        // Before the first pane, which may start Claude Code or Codex.
        app.reconcile_claude_plugin();
        app.reconcile_codex_shell();
        crate::startup_trace::mark(crate::startup_trace::Phase::AppBuilt);
        let result: Result<()> = match app.start_first_pane_before_launch() {
            Ok(_) => event_loop.run_app(&mut app).map_err(Into::into),
            Err(error) => {
                log::error!("{error}");
                Err(anyhow::anyhow!(error))
            }
        };
        app.runtime_tracker.set_phase("exiting");
        // Pasted screenshots are captured screen content, so they do not outlive
        // the session that produced them. Runs on the error path too — an exit
        // caused by a failure is exactly when leftovers would go unnoticed.
        // PNGs still being encoded are stopped and kept first, so cleanup
        // takes them too.
        if let Some(encoder) = app.png_encoder.take() {
            for encoded in encoder.close() {
                let _ = app.pasted_images.keep_png(encoded);
            }
        }
        app.pasted_images.cleanup();
        // So are the copies handed to the image viewer, and the store closes
        // so an open still on its way cannot leave a new one.
        app.viewer_copies
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .close();
        // And the copies handed to the document viewer and the video
        // player; a copy on its way stops first.
        app.document_copies.close();
        app.video_copies.close();
        if let Err(error) = &result {
            app.runtime_tracker.record_exit(&error.to_string());
        }
        app.runtime_tracker.stop();
        crate::notifications::flush_desktop_notifications(std::time::Duration::from_millis(250));
        result
    }

    /// The shell the focused pane has effectively entered (e.g. the
    /// user opened pwsh then typed `wsl`) — its argv + cwd, so a split can clone
    /// it in the same directory instead of a fresh default shell. Reads the
    /// latest background process snapshot, so a split keystroke never performs
    /// process enumeration on the event-loop thread. `None` before the first
    /// snapshot or for a plain pane; the split then clones the pane's launch
    /// command.
    fn focused_foreground_shell(
        &mut self,
        ws: &mut WindowState,
    ) -> Option<kettle_remote::ShellLaunch> {
        let (pane_id, pid, pane_cwd) = {
            let pane_id = ws.mux.active_focus()?;
            let pane = ws.mux.panes.get(&pane_id)?;
            (
                pane_id,
                pane.term.child_pid()?,
                pane.term.reported_current_dir(),
            )
        };
        let mut shell = self
            .remote_applied
            .get(&(pane_id, pid))?
            .foreground_shell
            .clone()?;
        // Assert the interactive-shell contract at the boundary.
        // The detector already rejects one-shot helpers, but re-checking here
        // means a split can never clone a non-interactive argv into a dead pane —
        // `None` routes the caller to `Mux::split`, which clones the pane's own
        // launch shell (falling back to the configured default).
        if !kettle_remote::shell_launch_is_interactive(&shell.argv) {
            return None;
        }
        // Prefer the pane's OSC 7-reported cwd — where the user actually is (e.g.
        // the WSL bash dir) — over the detected process's cwd, which Windows
        // sysinfo typically can't read for `wsl.exe` (it returned None, so the
        // split fell back to WSL's home `~`). `launch_cwd` carries this via
        // `wsl --cd`, which accepts both `/mnt/c/...` and native Linux paths.
        if pane_cwd.is_some() {
            shell.cwd = pane_cwd;
        }
        Some(shell)
    }

    fn waker(&self) -> kettle_core::Waker {
        let p = self.proxy.clone();
        let pending = self.wake_pending.clone();
        Arc::new(move || {
            // Enqueue at most ONE Wakeup per paint window. A queued Wakeup
            // re-checks every pane's output generation when it lands, so every
            // enqueue past the first is pure event-loop spam (a flood would
            // send one per 64KiB read). `swap` returning false means this call
            // owns the enqueue; the Wakeup arm reopens the latch before it
            // reads generations, so nothing is ever missed. Events the proxy
            // queues on the crossbeam channel before waking are drained by
            // that same pending Wakeup, so suppression never delays them.
            if !pending.swap(true, std::sync::atomic::Ordering::AcqRel) {
                let _ = p.send_event(UserEvent::Wakeup);
            }
        })
    }

    fn sync_output_frame_budget(&self, ws: &mut WindowState, force: bool) {
        let Some(window) = ws.window.clone() else {
            ws.output_monitor = None;
            ws.output_frame_budget = OUTPUT_FRAME_BUDGET;
            return;
        };
        // `current_monitor()` is the cheap identity probe. Querying the mode's
        // refresh rate is the expensive part on Windows, so same-monitor move
        // and resize storms share the rate limiter while a true same-DPI
        // monitor transition refreshes immediately.
        let monitor = window.current_monitor();
        let monitor_changed = monitor != ws.output_monitor;
        if monitor_changed {
            ws.output_monitor = monitor.clone();
        }
        let force = force || monitor_changed;
        let now = std::time::Instant::now();
        if monitor_refresh_probe_wait(ws.output_refresh_probe_at, now, force).is_some() {
            // Preserve one trailing probe so a quick same-DPI move from a
            // 60 Hz to a 165 Hz monitor cannot be lost to the rate limiter.
            ws.output_refresh_probe_pending = true;
            return;
        }
        ws.output_refresh_probe_at = Some(now);
        ws.output_refresh_probe_pending = false;
        let refresh_millihertz = monitor.and_then(|monitor| monitor.refresh_rate_millihertz());
        let next = output_frame_budget_for_refresh(refresh_millihertz);
        if next != ws.output_frame_budget {
            log::debug!(
                "window {} output pacing changed: refresh={refresh_millihertz:?} mHz, \
                 budget={next:?}",
                ws.seq
            );
            ws.output_frame_budget = next;
            // A monitor move begins a new pacing regime. Do not carry the old
            // monitor's sustained-flood tier into the first frame on the new
            // display.
            ws.flood_paints = 0;
        }
    }

    fn cell_px(&self, ws: &WindowState) -> (u16, u16) {
        ws.renderer
            .as_ref()
            .map(|r| {
                (
                    r.cell_w.round().clamp(1.0, u16::MAX as f32) as u16,
                    r.cell_h.round().clamp(1.0, u16::MAX as f32) as u16,
                )
            })
            .unwrap_or((8, 16))
    }

    /// Hide the OS mouse cursor; idempotent. Called when the user starts
    /// typing if `mouse-hide-while-typing` is on. The cursor reappears on
    /// the next mouse move or window-enter event.
    fn hide_mouse_cursor(&mut self, ws: &mut WindowState) {
        if ws.mouse_hidden || !self.cfg.mouse_hide_while_typing {
            return;
        }
        if let Some(w) = &ws.window {
            w.set_cursor_visible(false);
            ws.mouse_hidden = true;
        }
    }

    /// Show the OS mouse cursor; idempotent. Called whenever the mouse
    /// moves or re-enters the window.
    fn show_mouse_cursor(&mut self, ws: &mut WindowState) {
        if !ws.mouse_hidden {
            return;
        }
        if let Some(w) = &ws.window {
            w.set_cursor_visible(true);
            ws.mouse_hidden = false;
        }
    }

    /// The resize cursor for a split of the given
    /// orientation. A Horizontal split places panes side-by-side (a vertical
    /// divider you drag left/right → column resize); a Vertical split stacks
    /// them (a horizontal divider you drag up/down → row resize).
    fn resize_cursor_for(dir: Dir) -> CursorIcon {
        match dir {
            Dir::Horizontal => CursorIcon::ColResize,
            Dir::Vertical => CursorIcon::RowResize,
        }
    }

    /// A SplitDrag for the divider seam under content-area pixel
    /// `(px, py)`, or None. Used to start a drag-to-resize on left-press.
    fn split_drag_at(&self, ws: &WindowState, area: Rect, px: f32, py: f32) -> Option<SplitDrag> {
        let seams = ws.mux.split_seams(ws.mux.active, area);
        let i = crate::mux::seam_at(&seams, px, py, SPLIT_SEAM_TOL)?;
        let s = &seams[i];
        Some(SplitDrag {
            path: s.path.clone(),
            dir: s.dir,
        })
    }

    fn press_split_divider(&self, ws: &mut WindowState, button: u8) -> bool {
        if button != 0 || self.pointer_modal_open(ws) {
            return false;
        }
        let Some(drag) =
            self.split_drag_at(ws, self.area(ws), ws.cursor.x as f32, ws.cursor.y as f32)
        else {
            return false;
        };
        let dir = drag.dir;
        ws.dragging_split = Some(drag);
        if let Some(window) = &ws.window {
            window.set_cursor(Self::resize_cursor_for(dir));
            window.request_redraw();
        }
        true
    }

    fn drag_split_divider(&mut self, ws: &mut WindowState) -> bool {
        if ws.dragging_split.is_none() {
            return false;
        }
        let (path, dir) = {
            let d = ws.dragging_split.as_ref().unwrap();
            (d.path.clone(), d.dir)
        };
        let area = self.area(ws);
        let seams = ws.mux.split_seams(ws.mux.active, area);
        match seams.iter().find(|s| s.path == path).map(|s| s.rect) {
            Some(rect) => {
                let ratio =
                    crate::mux::ratio_from_pos(rect, dir, ws.cursor.x as f32, ws.cursor.y as f32);
                ws.mux.set_split_ratio(ws.mux.active, &path, ratio);
                // The layout-tree
                // ratio changed, but each pane's PTY grid is resized
                // only by resize_all (the keyboard resize path reaches
                // it via handle_action's tail). Without this the child
                // TUIs keep their old cols/rows — rendering clipped —
                // until some unrelated event fires a resize.
                self.resize_all(ws);
                if let Some(w) = &ws.window {
                    w.set_cursor(Self::resize_cursor_for(dir));
                    w.request_redraw();
                }
            }
            None => ws.dragging_split = None,
        }
        true
    }

    /// The resize cursor to show while merely HOVERING a divider
    /// seam (no drag yet), or None when the cursor isn't over one. Suppressed
    /// while a modal owns the pointer. Cheap in the common single-pane case —
    /// `split_seams` returns empty when the tab root is a lone leaf.
    fn split_seam_hover_icon(&self, ws: &WindowState) -> Option<CursorIcon> {
        if self.any_modal_open(ws) {
            return None;
        }
        let area = self.area(ws);
        let seams = ws.mux.split_seams(ws.mux.active, area);
        let i = crate::mux::seam_at(
            &seams,
            ws.cursor.x as f32,
            ws.cursor.y as f32,
            SPLIT_SEAM_TOL,
        )?;
        Some(Self::resize_cursor_for(seams[i].dir))
    }

    /// Set the OS mouse-cursor icon, deduped against the last value pushed
    /// to the window. Called on CursorMoved (position changes the
    /// hit-test) and on ModifiersChanged (the modifier state gates the
    /// click-to-open affordance). The icon, and the hover state these hit
    /// tests set, are the physical pointer's: every hit test reads where it
    /// is, not where a control client last moved the cursor, and with the
    /// pointer outside the window there is nothing to set.
    fn sync_cursor_icon(&mut self, ws: &mut WindowState) {
        let Some(pointer) = ws.native_pointer else {
            return;
        };
        let control = std::mem::replace(&mut ws.cursor, pointer);
        let media_receipt_hovered = self.media_paste_receipt_hovered(ws);
        self.sync_cursor_icon_with_media_receipt_hover(ws, media_receipt_hovered);
        ws.cursor = control;
    }

    /// CursorMoved already computed the receipt hit test to update its hover
    /// timer. Reuse that answer instead of building a second full overlay for
    /// the same physical event.
    fn sync_cursor_icon_with_media_receipt_hover(
        &mut self,
        ws: &mut WindowState,
        media_receipt_action_hovered: bool,
    ) {
        // Browser / iTerm2 / Ghostty convention: the OS cursor turns into
        // a "pointing hand" while the user holds the same modifier that
        // would open a URL on click (Ctrl on Linux/Windows, Cmd on
        // macOS — winit's `super_key` maps Cmd) and the pointer sits on
        // a clickable link. Otherwise show the standard text-I-beam, the
        // affordance every modern terminal uses for "this surface accepts
        // mouse selection."
        //
        // Chrome surfaces (tab bar, open modal overlays) override that —
        // they're clickable, not selectable, so the I-beam there is
        // visually misleading. `chrome_cursor_icon` is the pure decision.
        //
        // Tab close-buttons (`✕`) override the chrome `Default` with
        // `Pointer` so the trailing glyph reads as a clickable
        // affordance rather than a character in the title. Recomputed
        // here every cursor-move so the chip-hover state in the
        // renderer stays in sync (the renderer reads
        // `tabbar.hovered_close_idx`, which we pass through in
        // `tab_bar()`).
        let bar = self.tab_bar(ws);
        let prior_hover = (
            ws.hovered_close_idx,
            ws.hovered_new_tab,
            ws.hovered_new_tab_menu,
        );
        ws.hovered_close_idx =
            hovered_close_button(&bar.segments, ws.cursor.x as f32, ws.cursor.y as f32);
        let (cursor_x, cursor_y) = (ws.cursor.x as f32, ws.cursor.y as f32);
        let new_tab_hover = new_tab_hover_target(&bar, cursor_x, cursor_y);
        ws.hovered_new_tab = new_tab_hover == Some(NewTabHoverTarget::Plus);
        ws.hovered_new_tab_menu = new_tab_hover == Some(NewTabHoverTarget::Menu);
        if prior_hover
            != (
                ws.hovered_close_idx,
                ws.hovered_new_tab,
                ws.hovered_new_tab_menu,
            )
            && let Some(window) = &ws.window
        {
            window.request_redraw();
        }
        let close_hover = tab_close_hover_icon(ws.hovered_close_idx.is_some());
        let new_tab_hover_icon =
            (ws.hovered_new_tab || ws.hovered_new_tab_menu).then_some(CursorIcon::Pointer);
        let confirm_hover = if self.confirm_button_hovered(ws) {
            Some(CursorIcon::Pointer)
        } else {
            None
        };
        let search_hover = self.search_geometry(ws).and_then(|geometry| {
            let (x, y) = (ws.cursor.x as f32, ws.cursor.y as f32);
            geometry
                .hit_test(x, y)
                .map(|control| {
                    if control == kettle_render::SearchControl::Editor {
                        CursorIcon::Text
                    } else {
                        CursorIcon::Pointer
                    }
                })
                .or_else(|| rect_contains(geometry.rect, x, y).then_some(CursorIcon::Default))
        });
        let media_receipt_hover = media_receipt_action_hovered.then_some(CursorIcon::Pointer);
        // A settled card under the pointer takes a click: the pointing hand.
        let card_hover = self.note_card_hover(ws).then_some(CursorIcon::Pointer);
        let chrome =
            chrome_cursor_icon(self.cursor_in_chrome_band(ws), self.pointer_modal_open(ws));
        // A live tab-drag owns the cursor, FIRST in the chain. Mid-drag the
        // pointer can briefly cross another tab's ✕ hit-zone or a split seam,
        // and without the priority the icon would flicker Pointer/resize while
        // the user is holding a tab. Restore is automatic because `want` is
        // recomputed from live state on every cursor move, so the next event
        // after the FSM returns to `Idle` falls through to the rules below.
        let drag_icon = tab_drag_cursor_icon(&ws.detach_drag);
        // A split divider under the cursor shows the resize cursor
        // (after chrome/close-button, before the link-pointer / I-beam default).
        let toast_hover = self.update_toast_projection(ws).and_then(|toast| {
            let (x, y) = (ws.cursor.x as f32, ws.cursor.y as f32);
            rect_contains(toast.geometry.rect, x, y).then(|| {
                if rect_contains(toast.geometry.details, x, y)
                    || rect_contains(toast.geometry.close, x, y)
                {
                    CursorIcon::Pointer
                } else {
                    CursorIcon::Default
                }
            })
        });
        let want = drag_icon
            .or(toast_hover)
            .or(close_hover)
            .or(new_tab_hover_icon)
            .or(confirm_hover)
            .or(search_hover)
            .or(media_receipt_hover)
            .or(chrome)
            // A card takes a press before a split seam does, so its hand
            // wins where the two overlap.
            .or(card_hover)
            // A lane takes every press over it.
            .or_else(|| self.lane_cursor_icon(ws))
            .or_else(|| self.split_seam_hover_icon(ws))
            .unwrap_or_else(|| {
                let want_pointer = (ws.mods.control_key() || ws.mods.super_key())
                    && self.link_at_cursor(ws).is_some();
                if want_pointer {
                    CursorIcon::Pointer
                } else {
                    CursorIcon::Text
                }
            });
        let want = if ws.preview_overlay.is_some() && !self.non_search_modal_open(ws) {
            self.lane_cursor_icon(ws).unwrap_or(CursorIcon::Default)
        } else {
            want
        };
        if ws.last_cursor_icon != Some(want)
            && let Some(w) = &ws.window
        {
            w.set_cursor(want);
            ws.last_cursor_icon = Some(want);
        }
    }

    fn report_input_result(&mut self, result: PaneInputResult) {
        let Some(message) = input_rejection_message(result) else {
            return;
        };
        let now = std::time::Instant::now();
        if should_notify_input_rejection(self.last_input_rejection_notice, result, now) {
            log::warn!("terminal input rejected ({result:?}): {message}");
            let tr = &self.ui_text;
            fire_notify(
                tr.text(kettle_i18n::Text::NotifyTitleInputNotDelivered),
                input_rejection_text(result, tr).unwrap_or(message),
            );
            self.last_input_rejection_notice = Some((result, now));
        }
    }

    /// Deliver user-authored bytes with the same selection, cursor, broadcast,
    /// read-only, and scroll behavior for keyboard and IME commit paths.
    fn write_terminal_input(&mut self, ws: &mut WindowState, bytes: &[u8]) -> PaneInputResult {
        self.sync_pane_focus_reports(ws);
        if bytes.is_empty() {
            return PaneInputResult::Queued;
        }
        self.clear_selection_on_input(ws);
        self.reset_blink_phase(ws);
        self.hide_mouse_cursor(ws);
        // Queued behind a paste still being prepared, it is delivered in
        // order when that paste is.
        if self.defer_terminal_input(ws, None, bytes, true, false) {
            return PaneInputResult::Queued;
        }
        let input_time = std::time::Instant::now();
        let result = if ws.mux.is_broadcast_on() {
            self.broadcast_input(ws, bytes, self.cfg.scroll_on_keystroke, input_time)
        } else if let Some(pane_id) = ws.mux.active_focus() {
            let Some(pane) = ws.mux.panes.get(&pane_id) else {
                return PaneInputResult::Queued;
            };
            let result = pane.feed_input(bytes);
            if result.is_queued() {
                ws.last_typed = Some(input_time);
            }
            if result.is_queued()
                && self.cfg.scroll_on_keystroke
                && let Ok(mut term) = pane.term.term.lock()
            {
                term.scroll_display(Scroll::Bottom);
            }
            Self::dismiss_media_paste_receipt_after_input(ws, pane_id, result);
            result
        } else {
            PaneInputResult::Queued
        };
        self.report_input_result(result);
        result
    }

    fn write_terminal_key_event(
        &mut self,
        ws: &mut WindowState,
        event: &winit::event::KeyEvent,
        prepare_input: bool,
    ) {
        if self.defer_terminal_input(ws, Some(event), &[], prepare_input, false) {
            if prepare_input {
                self.clear_selection_on_input(ws);
                self.reset_blink_phase(ws);
                self.hide_mouse_cursor(ws);
            }
            return;
        }
        self.sync_pane_focus_reports(ws);
        let mods = pty_key_modifiers(ws, &event.logical_key);
        let modified_enter = key_is_modified_enter(&event.logical_key, mods);
        if !prepare_input {
            let recipients = ws
                .terminal_key_recipients
                .remove(&event.physical_key)
                .unwrap_or_default();
            let cfg = &self.cfg;
            let mut result = PaneInputResult::Queued;
            for mux in
                std::iter::once(&ws.mux).chain(self.windows.values().map(|window| &window.mux))
            {
                result = result.merge(mux.release_key_to(
                    &recipients,
                    cfg.modify_other_keys,
                    modified_enter,
                    |mode| encode_key_event_for_mode(cfg, event, mods, mode),
                ));
            }
            self.report_input_result(result);
            return;
        }
        if !ws.mux.is_broadcast_on() {
            let mode = ws
                .mux
                .focused()
                .map(|pane| pane.effective_key_mode(self.cfg.modify_other_keys, modified_enter))
                .unwrap_or_else(kettle_core::TermMode::empty);
            let Some(bytes) = encode_key_event_for_mode(&self.cfg, event, mods, mode) else {
                return;
            };
            let result = self.write_terminal_input(ws, &bytes);
            if result.is_queued()
                && !bytes.is_empty()
                && let Some(pane_id) = ws.mux.active_focus()
            {
                ws.terminal_key_recipients
                    .entry(event.physical_key)
                    .or_default()
                    .insert(pane_id);
            }
            return;
        }

        if prepare_input {
            self.clear_selection_on_input(ws);
            self.reset_blink_phase(ws);
            self.hide_mouse_cursor(ws);
        }
        let input_time = prepare_input.then(std::time::Instant::now);
        let scroll_to_bottom = prepare_input && self.cfg.scroll_on_keystroke;
        let scope = ws.mux.broadcast.clone();
        let cfg = &self.cfg;
        let receipt_pane = prepare_input
            .then(|| Self::media_paste_receipt_pane(ws))
            .flatten();
        let (mut delivery, recipients) = ws.mux.broadcast_encoded(
            self.cfg.modify_other_keys,
            modified_enter,
            scroll_to_bottom,
            receipt_pane,
            |mode| encode_key_event_for_mode(cfg, event, mods, mode),
        );
        ws.terminal_key_recipients
            .entry(event.physical_key)
            .or_default()
            .extend(recipients);
        Self::dismiss_media_paste_receipt_after_delivery(ws, delivery);
        if let Some(input_time) = input_time {
            stamp_accepted_input(&mut ws.last_typed, delivery, input_time);
        }
        if Mux::scope_crosses_windows(&scope) {
            for other in self.windows.values_mut() {
                let receipt_pane = prepare_input
                    .then(|| Self::media_paste_receipt_pane(other))
                    .flatten();
                let (foreign, recipients) = other.mux.broadcast_encoded_foreign(
                    &scope,
                    self.cfg.modify_other_keys,
                    modified_enter,
                    scroll_to_bottom,
                    receipt_pane,
                    |mode| encode_key_event_for_mode(cfg, event, mods, mode),
                );
                ws.terminal_key_recipients
                    .entry(event.physical_key)
                    .or_default()
                    .extend(recipients);
                if scroll_to_bottom
                    && foreign.accepted
                    && let Some(window) = &other.window
                {
                    window.request_redraw();
                }
                Self::dismiss_media_paste_receipt_after_delivery(other, foreign);
                if let Some(input_time) = input_time {
                    stamp_accepted_input(&mut other.last_typed, foreign, input_time);
                }
                delivery.result = delivery.result.merge(foreign.result);
                delivery.accepted |= foreign.accepted;
            }
        }
        self.report_input_result(delivery.result);
    }

    fn commit_ime_text(&mut self, ws: &mut WindowState, text: &str, event_loop: &ActiveEventLoop) {
        if text.is_empty() {
            return;
        }
        let key = Key::Character(text.into());
        if ws.context_menu.is_some() || ws.vi_mode.is_some() || ws.hint_state.is_some() {
            return;
        }
        // The modal handlers below filter typed text by the modifiers that
        // produced it. A committed composition has none: the input method
        // decides when to commit, and whatever is latched at that instant did
        // not type the phrase. Committing with `⌘Space` (switch input source)
        // would otherwise drop the whole thing. Present the handlers with the
        // empty modifier set for the duration and restore it afterwards, so
        // `ws.mods` stays truthful for the next real keystroke.
        let latched_mods = ws.mods;
        ws.mods = ModifiersState::empty();
        // The modal outranks whatever raised it. Rebinding a key onto a chord
        // that is already taken raises a confirm dialog from inside the
        // Settings overlay. If the Settings arms below claimed the keyboard
        // first, the question could not be answered, by `y`/`n` or by anything
        // else, while the panel that asked it stayed open. The dialog's own
        // handler (the named-key path) reads `y`/`n`; this arm only has to
        // stop the text from going somewhere else.
        if ws.confirm_dialog.is_some() {
            ws.mods = latched_mods;
            return;
        } else if ws.palette_input.is_some() {
            self.palette_key(ws, &key, Some(text), event_loop);
        } else if ws.theme_picker_input.is_some() {
            self.theme_picker_key(ws, &key, Some(text));
        } else if ws.settings_nav.is_some() && ws.settings_text_edit.is_some() {
            self.settings_text_key(ws, &key, Some(text));
        } else if ws.settings_nav.is_some() {
            ws.mods = latched_mods;
            return;
        } else if ws.layout_picker_input.is_some() {
            self.layout_picker_key(ws, &key, Some(text));
        } else if ws.ssh_input.is_some() {
            self.ssh_key(ws, &key, Some(text));
        } else if ws.editing_title.is_some() {
            // Route through the shared helper rather than extending the buffer
            // inline, so an IME commit cannot bypass the 4 KiB cap the typed
            // path enforces.
            if let Some(accepted) = crate::modal_input::accept_committed_text(text)
                && let Some(state) = ws.editing_title.as_mut()
            {
                crate::modal_input::push_text(&mut state.input, accepted);
            }
        } else if ws.search.open {
            self.search_key(ws, &key, None, Some(text));
        } else {
            ws.mods = latched_mods;
            let _ = self.write_terminal_input(ws, text.as_bytes());
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
            return;
        }
        ws.mods = latched_mods;
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
    }

    /// Content area for panes (excludes the tab bar, status bar, and
    /// passive update banner), in physical pixels.
    /// Terminator parity: hit-test for a click in any pane's per-pane titlebar region.
    /// Returns the pane id whose titlebar was clicked, or None if
    /// the click fell outside titlebar regions. Honors the
    /// `cfg.show_titlebar` gate + the `cfg.title_at_bottom` flip.
    /// Used by the click handler to dispatch EditPaneTitle on the
    /// clicked pane.
    fn pane_at_titlebar_click(&self, ws: &WindowState, px: f32, py: f32) -> Option<u64> {
        if !self.cfg.show_titlebar {
            return None;
        }
        let active = ws.mux.active;
        let rects = ws.mux.leaves(active, self.area(ws));
        if rects.len() < 2 {
            // Single-pane tab: titlebar isn't rendered (the renderer's
            // titlebar gate requires `show_titlebar` and more than one pane).
            return None;
        }
        let bar_h = ws.renderer.as_ref().map(|r| r.cell_h + 6.0).unwrap_or(20.0);
        pane_titlebar_hit(px, py, &rects, self.cfg.title_at_bottom, bar_h)
    }

    /// The cell size in physical pixels: the renderer's, or the one measured
    /// before it existed.
    fn cell_size(ws: &WindowState) -> Option<(f32, f32)> {
        ws.renderer
            .as_ref()
            .map(|r| (r.cell_w, r.cell_h))
            .or(ws.startup_cell)
    }

    /// The surface size in physical pixels: the renderer's, or the one chosen
    /// before it existed.
    fn surface_size(ws: &WindowState) -> Option<(u32, u32)> {
        ws.renderer
            .as_ref()
            .map(|r| r.surface_size())
            .or(ws.startup_surface)
    }

    /// The first window's surface in physical pixels, before the renderer
    /// exists. An axis `window-width` or `window-height` sets holds exactly
    /// that many cells at the startup cell size. An axis the config leaves
    /// alone keeps the default window's pixel size, fitted to the monitor
    /// when neither is set. `None` before the cell is measured.
    fn startup_surface(
        &self,
        ws: &WindowState,
        monitor: Option<StartupMonitor>,
    ) -> Option<(u32, u32)> {
        let cell = Self::cell_size(ws)?;
        let scale = monitor.map_or(1.0, |m| m.scale);
        let (default_w, default_h) = startup_inner_size(&self.cfg, monitor);
        let physical = |logical: f64| (logical * scale).round().max(1.0) as u32;
        let cols = self.cfg.window_width.map(|cols| {
            cols.clamp(
                kettle_config::WINDOW_WIDTH_MIN,
                kettle_config::WINDOW_WIDTH_MAX,
            )
        });
        let rows = self.cfg.window_height.map(|rows| {
            rows.clamp(
                kettle_config::WINDOW_HEIGHT_MIN,
                kettle_config::WINDOW_HEIGHT_MAX,
            )
        });
        let (exact_w, exact_h) = surface_for_grid(
            (
                cols.unwrap_or(STARTUP_DEFAULT_COLS) as usize,
                rows.unwrap_or(STARTUP_DEFAULT_ROWS) as usize,
            ),
            cell,
            (self.cfg.padding_x, self.cfg.padding_y),
            self.content_bands(ws),
            self.cfg.tab_bar_pos,
            self.cfg.status_bar,
            self.cfg.tab_bar_width,
        );
        Some((
            if cols.is_some() {
                exact_w
            } else {
                physical(default_w)
            },
            if rows.is_some() {
                exact_h
            } else {
                physical(default_h)
            },
        ))
    }

    fn prepare_startup_fonts(&mut self, ws: &mut WindowState, startup_scale: f32) {
        let fonts = match ws.startup_fonts.take() {
            Some(fonts) if fonts.matches(&self.cfg, startup_scale) => fonts,
            Some(fonts) => fonts.remeasure(&self.cfg, startup_scale),
            None => match self.font_preload.take() {
                Some(preload) => preload.finish(&self.cfg, startup_scale),
                None => kettle_render::StartupFonts::load(&self.cfg, startup_scale),
            },
        };
        ws.startup_cell = Some(fonts.cell);
        ws.startup_fonts = Some(fonts);
    }

    fn start_first_pane_before_launch(&mut self) -> Result<bool, String> {
        #[cfg(not(target_os = "macos"))]
        {
            let _ = self.pre_launch_eligible;
            Ok(false)
        }
        #[cfg(target_os = "macos")]
        {
            let decline = crate::startup_trace::note_pre_launch_decline;
            if !self.pre_launch_eligible {
                decline("ineligible".to_owned());
                return Ok(false);
            }
            let Some(monitor) = crate::macos_display::startup_display() else {
                decline("display".to_owned());
                return Ok(false);
            };
            crate::startup_trace::mark(crate::startup_trace::Phase::DisplayRead);
            let Some(mut ws) = self.windows.remove(&1) else {
                decline("window".to_owned());
                return Ok(false);
            };
            self.prepare_startup_fonts(&mut ws, monitor.scale as f32);
            let surface = self.startup_surface(&ws, Some(monitor));
            let result = if surface.is_some_and(|size| fits_monitor(size, Some(monitor))) {
                ws.startup_surface = surface;
                self.spawn_first_tab(&mut ws, self.launch_override)
                    .map(|()| {
                        ws.pre_launch = Some(monitor);
                        crate::startup_trace::mark(crate::startup_trace::Phase::PaneSpawned);
                        crate::startup_trace::note_path(
                            crate::startup_trace::StartupPath::PreLaunch,
                        );
                        true
                    })
            } else {
                // Keep the font system for the resumed path's measurement.
                ws.startup_cell = None;
                decline(match surface {
                    Some((width, height)) => format!(
                        "fit surface={width}x{height} monitor={:.0}x{:.0} scale={}",
                        monitor.logical.0, monitor.logical.1, monitor.scale
                    ),
                    None => "fit surface=none".to_owned(),
                });
                Ok(false)
            };
            self.windows.insert(1, ws);
            result
        }
    }

    /// Start the first tab at the current startup grid: the `-e` command and
    /// working directory when given, else the shell.
    fn spawn_first_tab(
        &mut self,
        ws: &mut WindowState,
        launch_override: bool,
    ) -> Result<(), String> {
        let area = self.area(ws);
        let (cols, rows) = self.grid_of(ws, area);
        let geometry = self.pty_geometry_for_grid(ws, cols, rows);
        if launch_override {
            let argv = self.startup.command.take().unwrap_or_default();
            let cwd = self
                .startup
                .cwd
                .take()
                .map(|p| p.to_string_lossy().into_owned());
            ws.mux
                .new_tab_with_geometry(&self.cfg, geometry, self.waker(), &argv, cwd.as_deref())
                .map_err(|e| format!("failed to spawn `-e` command: {e}"))
        } else {
            ws.mux
                .new_tab_geometry(&self.cfg, geometry, self.waker())
                .map_err(|e| format!("failed to spawn shell: {e}"))
        }
    }

    fn content_bands(&self, ws: &WindowState) -> ContentBands {
        ContentBands {
            tab_bar_h: self.tab_bar_h(ws),
            status_bar_h: self.status_bar_h(ws),
            vertical_title_edit_h: self.vertical_title_edit_h(ws),
        }
    }

    fn area(&self, ws: &WindowState) -> Rect {
        self.area_with_bands(ws, self.content_bands(ws))
    }

    fn area_with_bands(&self, ws: &WindowState, bands: ContentBands) -> Rect {
        let surface = Self::surface_size(ws).unwrap_or((800, 600));
        // Delegate to the pure helper, threading
        // `cfg.tab_bar_width` so a user-configured strip width
        // is honored.
        let mut area = content_rect_for_with_strip(
            surface,
            bands,
            self.cfg.tab_bar_pos,
            self.cfg.status_bar,
            self.cfg.tab_bar_width,
        );
        area.3 = (area.3 - self.search_bar_h(ws)).max(0.0);
        area
    }

    /// Lift the drag ghost toward the same threshold that triggers tear-off.
    /// A window without its own active drag has no lift.
    fn tear_lift_for(&self, ws: &WindowState) -> f32 {
        if !self.cfg.detachable_tabs {
            return 0.0;
        }
        match ws.detach_drag {
            crate::detach::DragState::DraggingInside { .. }
            | crate::detach::DragState::DraggingOutside { .. } => self
                .dock_band(ws)
                .map(|band| {
                    tear_lift_ratio(
                        ws.cursor.x as f32,
                        ws.cursor.y as f32,
                        band,
                        1.5 * self.tab_bar_h(ws),
                    )
                })
                .unwrap_or(0.0),
            _ => 0.0,
        }
    }

    /// The columns and rows that fit in `terminal`, a pane's terminal (or a
    /// whole area with nothing carved from it), inside its padding.
    fn grid_of(&self, ws: &WindowState, terminal: Rect) -> (usize, usize) {
        let (cw, ch) = Self::cell_size(ws).unwrap_or((8.0, 16.0));
        let (_, _, w, h) = terminal;
        grid_dims_px((w, h), (cw, ch), (self.cfg.padding_x, self.cfg.padding_y))
    }

    fn pty_geometry_for_grid(&self, ws: &WindowState, columns: usize, rows: usize) -> PtyGeometry {
        let fractional_cell = Self::cell_size(ws).unwrap_or((8.0, 16.0));
        let (pixel_width, pixel_height) = pty_pixel_size(fractional_cell, (columns, rows));
        PtyGeometry::new(columns, rows, pixel_width, pixel_height)
    }

    fn pty_geometry_for_terminal(&self, ws: &WindowState, terminal: Rect) -> PtyGeometry {
        let (columns, rows) = self.grid_of(ws, terminal);
        self.pty_geometry_for_grid(ws, columns, rows)
    }

    fn prospective_split_geometry(
        &self,
        ws: &WindowState,
        dir: Dir,
        new_first: bool,
    ) -> PtyGeometry {
        // The new pane's leaf in a tab one pane larger, with no lane of its own.
        let area = self.area(ws);
        let leaf = ws
            .mux
            .prospective_split_rect(dir, new_first, area)
            .unwrap_or(area);
        let metrics = self
            .layout_style(ws)
            .metrics(ws.mux.active_pane_count().saturating_add(1));
        let terminal = crate::pane_partition::partition_leaf(leaf, metrics, None).terminal;
        self.pty_geometry_for_terminal(ws, terminal)
    }

    fn restore_geometries(
        &self,
        ws: &WindowState,
        session: &crate::session::Session,
    ) -> Vec<Vec<PtyGeometry>> {
        // A restored tab has no lanes: each pane's terminal is its leaf less
        // its titlebar.
        let bands = self.content_bands(ws).for_restore(
            self.cfg.tab_bar,
            session.tabs.len(),
            Self::cell_size(ws).map_or(16.0, |(_, height)| height),
        );
        let area = self.area_with_bands(ws, bands);
        let style = self.layout_style(ws);
        let terminal = |leaf: Rect, panes: usize| {
            crate::pane_partition::partition_leaf(leaf, style.metrics(panes), None).terminal
        };
        session
            .tabs
            .iter()
            .map(|tab| {
                let rectangles = tab.root.leaf_rects(area);
                let pane_count = rectangles.len();
                let mut geometries: Vec<_> = rectangles
                    .into_iter()
                    .map(|leaf| self.pty_geometry_for_terminal(ws, terminal(leaf, pane_count)))
                    .collect();
                if tab.zoomed
                    && let Some(focused_index) =
                        (!geometries.is_empty()).then(|| tab.focus.min(geometries.len() - 1))
                    && let Some(focused) = geometries.get_mut(focused_index)
                {
                    *focused = self.pty_geometry_for_terminal(ws, terminal(area, 1));
                }
                geometries
            })
            .collect()
    }

    fn pane_snapshots_match_current_layout(&self, ws: &WindowState) -> bool {
        if ws.renderer.is_none() || ws.pane_snapshots.len() != ws.pane_snapshot_keys.len() {
            return false;
        }
        let area = self.area(ws);
        let layout = self.pane_layout(ws, area);
        let current = layout.iter().filter_map(|(id, partition)| {
            let pane = ws.mux.panes.get(id)?;
            let (columns, screen_lines) = self.grid_of(ws, partition.terminal);
            Some((*id, pane.term.output_generation(), columns, screen_lines))
        });
        pane_snapshot_keys_match(&ws.pane_snapshot_keys, current)
            && ws
                .pane_snapshot_keys
                .iter()
                .zip(&ws.pane_snapshots)
                .all(|((id, ..), snap)| {
                    ws.mux.panes.get(id).is_some_and(|pane| {
                        snap.captures_card_marks() == pane.inline_cards.needs_marks()
                    })
                })
    }

    fn pane_cursor_layer_eligible(ws: &WindowState) -> bool {
        use kettle_core::CursorShape;
        use kettle_core::TermMode;
        ws.vi_mode.is_none()
            && ws
                .mux
                .active_focus()
                .and_then(|id| ws.mux.panes.get(&id))
                .is_some_and(|pane| {
                    let Ok(term) = pane.term.term.try_lock() else {
                        return false;
                    };
                    let content = term.renderable_content();
                    let row = i64::from(content.cursor.point.line.0)
                        .saturating_add(i64::try_from(content.display_offset).unwrap_or(i64::MAX));
                    !content.mode.contains(TermMode::VI)
                        && content.cursor.shape != CursorShape::Hidden
                        && (0..i64::try_from(term.grid().screen_lines()).unwrap_or(0))
                            .contains(&row)
                })
    }

    fn pane_cursor_blinking(&self, ws: &WindowState, use_pane_snapshots: bool) -> bool {
        pane_cursor_blinking_with(
            ws.mux.active_focus(),
            &ws.pane_snapshot_keys,
            &ws.pane_snapshots,
            use_pane_snapshots,
            |id| {
                ws.mux
                    .panes
                    .get(&id)
                    .map(|pane| pane.term.cursor_blinking())
            },
        )
    }

    /// Which way `split_auto` should cut the focused pane.
    ///
    /// Terminator's rule is the pane's LONGER axis: a wide pane splits into a
    /// left/right pair, a tall one into top/bottom. Every user-facing
    /// description promises this (`docs/CONFIG.md` "pick by aspect ratio", the
    /// palette's "Split automatically", the context-menu row, and the default
    /// Ctrl+Shift+A binding).
    ///
    /// Ties go to a vertical cut, the same as `split_down`, so the default
    /// binding keeps that meaning where the aspect ratio favours neither side.
    fn auto_split_dir(&self, ws: &WindowState) -> Dir {
        let area = self.area(ws);
        let rect = ws
            .mux
            .active_focus()
            .and_then(|focus| {
                ws.mux
                    .leaves(ws.mux.active, area)
                    .into_iter()
                    .find(|(id, _)| *id == focus)
            })
            .map_or(area, |(_, leaf)| leaf);
        if rect.2 > rect.3 {
            Dir::Horizontal
        } else {
            Dir::Vertical
        }
    }

    /// How this window measures its panes: its cells and padding, and the
    /// titlebar a pane shows beside another.
    fn layout_style(&self, ws: &WindowState) -> LayoutStyle {
        let cell = Self::cell_size(ws).unwrap_or((8.0, 16.0));
        let padding = (self.cfg.padding_x, self.cfg.padding_y);
        let titlebar = self
            .cfg
            .show_titlebar
            .then(|| ws.renderer.as_ref().map(|r| r.cell_h + 6.0).unwrap_or(20.0));
        LayoutStyle::new(cell, padding, titlebar, self.cfg.title_at_bottom)
            .or_else(|| LayoutStyle::new((8.0, 16.0), padding, titlebar, self.cfg.title_at_bottom))
            .unwrap_or_else(LayoutStyle::fallback)
    }

    /// The active tab's panes laid out over `area`, each divided between its
    /// titlebar, terminal and preview lane.
    fn pane_layout(&self, ws: &WindowState, area: Rect) -> Vec<(u64, PanePartition)> {
        ws.mux.layout(ws.mux.active, area, self.layout_style(ws))
    }

    /// The partition of `pane_id` in the active tab.
    fn pane_partition(&self, ws: &WindowState, area: Rect, pane_id: u64) -> Option<PanePartition> {
        self.pane_layout(ws, area)
            .into_iter()
            .find(|(id, _)| *id == pane_id)
            .map(|(_, partition)| partition)
    }

    /// The terminal of `pane_id` in the active tab: its grid with the
    /// padding around it, clear of the titlebar and lane.
    fn pane_terminal(&self, ws: &WindowState, area: Rect, pane_id: u64) -> Option<Rect> {
        self.pane_partition(ws, area, pane_id)
            .map(|partition| partition.terminal)
    }

    /// The focused pane's terminal.
    fn focused_terminal(&self, ws: &WindowState, area: Rect) -> Option<Rect> {
        self.pane_terminal(ws, area, ws.mux.active_focus()?)
    }

    /// Open a link taken from pane `pane`'s output. A local file link from a
    /// remote pane is refused with a notification, since the path names a file
    /// on that machine; a pane that has gone counts as remote. Other links open
    /// through [`Self::open_url`].
    fn open_pane_link(&mut self, ws: &mut WindowState, pane: Option<u64>, uri: &str) {
        let normalized = kettle_core::links::normalize_url(uri);
        let uri = normalized.as_str();
        let origin = pane
            .and_then(|id| ws.mux.panes.get(&id))
            .map(pane_path_origin);
        let tr = self.ui_text;
        match link_gate(uri, origin) {
            LinkGate::Open => self.open_url(ws, uri, LinkSource::Pane(origin)),
            LinkGate::Refuse => fire_notify(
                tr.text(kettle_i18n::Text::NotifyTitleRemoteFileNotOpened),
                &tr.notify_body_remote_file_not_opened(&link_display_name(uri, 48)),
            ),
        }
    }

    /// Open a URL. Unsafe URLs are refused, and Lua URL handlers may claim or
    /// rewrite it first. Then, for Terminator parity
    /// (terminatorlib/config.py:86-87 `use_custom_url_handler` +
    /// `custom_url_handler`), open it via the custom external program if
    /// configured and non-empty, else via the cross-platform `open` crate.
    /// The custom program is invoked as
    ///   <custom_url_handler> <uri>
    /// detached, so kettle doesn't block on the handler exiting.
    /// Errors log::warn.
    fn open_url(&mut self, ws: &WindowState, uri: &str, source: LinkSource) {
        if !kettle_core::links::is_safe_url(uri) {
            warn_refused_terminal_url(uri, |message| log::warn!("{message}"));
            return;
        }
        // Terminator plugin parity: fire
        // `LuaEvent::UrlClicked`. Fired before pattern-handler
        // dispatch so analytics / logging / workflow-trigger
        // plugins see ALL URL clicks, regardless of which
        // handler eventually opens them. The
        // `try_url_handler` chain below still owns the "actually
        // launch" decision; this event is observation-only.
        self.fire_lua_event(
            ws,
            crate::LuaEvent::UrlClicked(uri.to_string()),
            "url_clicked hook",
        );
        // Terminator plugin parity: Lua URL handlers get first dispatch. A
        // handler that claims the URL either handles it itself or returns a
        // URL for the custom-handler / system-open paths below.
        let outcome = self
            .lua_engine
            .as_ref()
            .map(|engine| engine.try_url_handler(uri))
            .unwrap_or(crate::lua::UrlHandlerOutcome::Fallthrough);
        // URL-handler callbacks are distinct from UrlClicked observers and can
        // queue the same side effects. Drain before any claimed-handler return.
        self.drain_lua_hook_commands(ws, "URL handler");
        // A handler may REWRITE the URL — that is the documented contract in
        // `docs/examples/init.lua` and the shape every example there uses.
        let rewritten;
        let uri = match &outcome {
            crate::lua::UrlHandlerOutcome::Handled => return,
            crate::lua::UrlHandlerOutcome::Open(target) => {
                // Re-check the allowlist. The handler is the user's own code,
                // but it is fed untrusted terminal text, so it must not be a
                // way to turn that text into a URL kettle would have refused.
                if !kettle_core::links::is_safe_url(target) {
                    log::warn!("lua url_handler returned an unsafe URL; refusing to open it");
                    return;
                }
                // A handler may turn a web link from a remote pane into a local
                // file link, or a confirmed file into another, so the pane's
                // gate applies to any different link it returns.
                if let LinkSource::Pane(origin) = source
                    && target != uri
                    && link_gate(target, origin) != LinkGate::Open
                {
                    log::warn!(
                        "lua url_handler returned a local file link for a pane whose files \
                         may be elsewhere; refusing to open it"
                    );
                    return;
                }
                rewritten = target.clone();
                rewritten.as_str()
            }
            crate::lua::UrlHandlerOutcome::Fallthrough => uri,
        };
        // A local file link is checked before anything opens it, the custom
        // handler included: a program or a shortcut is refused, since opening
        // it would run it or follow it elsewhere. One that does not decode to
        // a local path is refused here. The check looks at an untrusted path,
        // which can stall on a network mount, so it and the open run off the
        // UI thread.
        if uri
            .get(..7)
            .is_some_and(|scheme| scheme.eq_ignore_ascii_case("file://"))
        {
            let Some(path) = kettle_core::links::local_file_path(uri) else {
                log::warn!("refusing a file link that names no local path");
                return;
            };
            let handler = (self.cfg.use_custom_url_handler
                && !self.cfg.custom_url_handler.is_empty())
            .then(|| self.cfg.custom_url_handler.clone());
            spawn_file_link_opener(path, handler, self.ui_text);
            return;
        }
        if self.cfg.use_custom_url_handler && !self.cfg.custom_url_handler.is_empty() {
            let cmd = self.cfg.custom_url_handler.clone();
            system_opener::open_custom(&cmd, uri);
            return;
        }
        system_opener::open(uri, |e| {
            log::warn!("failed to open a terminal-supplied URL: {e}");
        });
    }

    /// Resize every pane's PTY to match its tile in the layout.
    fn resize_all(&mut self, ws: &mut WindowState) {
        // Never synthesize a layout from the helper fallbacks while the
        // renderer is temporarily absent during GPU recovery. Existing PTYs
        // retain their last good geometry; the atomic renderer rebuild calls
        // this once with the real surface and fractional cell metrics.
        if ws.renderer.is_none() {
            return;
        }
        let (cw, ch) = self.cell_px(ws);
        let fractional_cell = ws
            .renderer
            .as_ref()
            .map(|r| (r.cell_w, r.cell_h))
            .unwrap_or((f32::from(cw), f32::from(ch)));
        let area = self.area(ws);
        let mut plan: Vec<(u64, usize, usize, u16, u16)> = Vec::new();
        let style = self.layout_style(ws);
        for ti in 0..ws.mux.tabs.len() {
            // Each PTY gets the rows and columns of its pane's terminal, clear
            // of its titlebar and lane.
            for (id, partition) in ws.mux.layout(ti, area, style) {
                let (cols, rows) = self.grid_of(ws, partition.terminal);
                let (pixel_width, pixel_height) = pty_pixel_size(fractional_cell, (cols, rows));
                plan.push((id, cols, rows, pixel_width, pixel_height));
            }
        }
        let mut native_resize_failed = false;
        for (id, cols, rows, pixel_width, pixel_height) in plan {
            if let Some(p) = ws.mux.panes.get_mut(&id)
                && let Err(error) = p.term.try_resize_geometry(PtyGeometry::new(
                    cols,
                    rows,
                    pixel_width,
                    pixel_height,
                ))
            {
                native_resize_failed = true;
                log::warn!(
                    "pane {id} PTY resize failed; a bounded deadline retry is scheduled: {error:#}"
                );
            }
        }
        ws.pty_resize_retry
            .record_result(std::time::Instant::now(), native_resize_failed);
        // The window, its font or its splits changed what a zoomed lane
        // shows.
        self.lanes_moved(ws);
    }

    fn restore_scaled_zoom_font(&mut self, ws: &mut WindowState) {
        if let Some(prev) = ws.scaled_zoom_prev_font_size.take()
            && let Some(r) = ws.renderer.as_mut()
        {
            r.set_font_size(prev);
        }
    }

    /// Shared zoom transition for `Action::ToggleZoom` and
    /// `Action::ScaledZoom`. Both actions flip the SAME `ws.mux` zoom flag, so
    /// they must share `ws.scaled_zoom_prev_font_size`. Otherwise a plain
    /// `ToggleZoom` could un-zoom a pane that `ScaledZoom` had enlarged without
    /// restoring the font or clearing the stale baseline, leaving the pane
    /// un-zoomed but oversized and corrupting any later `ScaledZoom` call's
    /// save/restore pairing.
    ///
    /// `scale` is `Some(factor)` for `ScaledZoom`, which bumps the live font by
    /// `factor` on zoom-in and saves the pre-bump size the first time. It is
    /// `None` for plain `ToggleZoom`, which never bumps the font on zoom-in.
    /// Either action, on zoom-*out*, restores the font from
    /// `scaled_zoom_prev_font_size` and clears it if a scaled-zoom baseline is
    /// pending, so the pairing never desyncs whichever action toggled the flag.
    fn toggle_zoom_with_scale(&mut self, ws: &mut WindowState, scale: Option<f32>) {
        ws.mux.toggle_zoom();
        let now_zoomed = ws.mux.is_zoomed();
        if let Some(r) = ws.renderer.as_mut() {
            if now_zoomed {
                if let Some(factor) = scale {
                    // Baseline off the LIVE renderer size, not
                    // `self.cfg.font_size`. Increase/DecreaseFontSize only
                    // call `r.set_font_size` (they never write
                    // `cfg.font_size`), so a prior manual zoom left the
                    // config value stale — scaling from it would otherwise
                    // discard the user's manual zoom on restore.
                    let cur = r.font_size();
                    if ws.scaled_zoom_prev_font_size.is_none() {
                        ws.scaled_zoom_prev_font_size = Some(cur);
                    }
                    let new_size = (cur * factor).clamp(6.0, 96.0);
                    r.set_font_size(new_size);
                }
            } else if let Some(prev) = ws.scaled_zoom_prev_font_size.take() {
                r.set_font_size(prev);
            }
        }
        // Grid and pixel geometry must be derived after any scaled-zoom font
        // change. Resizing first would leave the PTY at the previous cell
        // metrics until an unrelated resize event, so TUIs and CSI 14 t would
        // disagree with the pixels actually rendered.
        self.resize_all(ws);
    }

    fn apply_window_resize(&mut self, ws: &mut WindowState, width: u32, height: u32) {
        // Never reconfigure a dead surface. A Resized event after a GPU device
        // loss would otherwise drive a surface configure on the lost device. The
        // grid resize below still runs so the layout is correct if the user
        // reopens.
        let gpu_lost = self.gpu.as_ref().is_some_and(|g| g.is_lost());
        if !gpu_lost
            && !ws.frame_recovery.renderer_rebuild_pending()
            && let Some(r) = ws.renderer.as_mut()
        {
            r.resize(width, height);
        }
        self.reclamp_context_menu(ws, (width as f32, height as f32));
        self.resize_all(ws);
        let show_chip = match self.cfg.resize_overlay {
            kettle_config::ResizeOverlayMode::Never => false,
            kettle_config::ResizeOverlayMode::Always => true,
            kettle_config::ResizeOverlayMode::AfterFirst => {
                ws.seen_first_resize
                    && ws.spawned_at.elapsed() > std::time::Duration::from_millis(1500)
            }
        };
        ws.seen_first_resize = true;
        if show_chip {
            let (cols, rows) = self.grid_of(ws, self.area(ws));
            ws.resize_overlay = Some((cols as u16, rows as u16, std::time::Instant::now()));
        }
    }

    fn commit_window_resize(&mut self, ws: &mut WindowState, width: u32, height: u32) {
        ws.frame_recovery.expedite(std::time::Instant::now());
        self.apply_window_resize(ws, width, height);
        // Record the new grid size into the asciicast trace.
        if self.recorder.is_some() {
            let (cols, rows) = self.grid_of(ws, self.area(ws));
            if let Some(rec) = self.recorder.as_mut() {
                rec.record_resize(cols as u16, rows as u16);
            }
        }
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
    }

    fn dpi_resize_renderer_ready(&self, ws: &WindowState) -> bool {
        ws.renderer.is_some()
            && !self.gpu.as_ref().is_some_and(|gpu| gpu.is_lost())
            && !ws.frame_recovery.renderer_rebuild_pending()
    }

    /// Fan one destructive drain of the shared PTY-output sidechannel out to
    /// every enabled consumer. Both redraw-time and close-time drains must use
    /// this path; otherwise whichever path receives a chunk first can starve
    /// recorder or Lua output hooks of the same bytes.
    fn dispatch_output_chunks(
        &mut self,
        _ws: &mut WindowState,
        output_chunks: Vec<(u64, Vec<u8>)>,
    ) {
        for (pane_id, bytes) in output_chunks {
            if let Some(rec) = self.recorder.as_mut() {
                rec.record_output(&bytes);
            }
            self.fire_lua_event(_ws, crate::LuaEvent::Output(pane_id, bytes), "output hook");
        }
        let body = self
            .ui_text
            .text(kettle_i18n::Text::NotifyBodyRecordingWriteFailed);
        self.sync_recording_state(_ws, body);
    }

    fn sync_session_log_failures(&self, ws: &WindowState) {
        for (&pane_id, pane) in &ws.mux.panes {
            let Some(failure) = pane.term.take_session_log_failure() else {
                continue;
            };
            let tr = &self.ui_text;
            let body = match failure {
                kettle_core::SessionLogFailure::Overloaded => {
                    tr.notify_body_session_log_overloaded(pane_id)
                }
                kettle_core::SessionLogFailure::IoError => tr.notify_body_session_log_io(pane_id),
            };
            fire_notify(
                tr.text(kettle_i18n::Text::NotifyTitleSessionLogStopped),
                &body,
            );
        }
    }

    /// Copy a pane's latched OSC 52 writes to the system clipboard, oldest
    /// first, when the config allows programs to copy.
    fn store_osc52_writes(&mut self, writes: Vec<(ClipboardType, String)>) {
        for (target, s) in writes {
            // Normalize complete card clusters before clipping so a
            // byte limit cannot leave a partial placeholder behind.
            if self.cfg.osc52.can_copy()
                && let Some(cb) = &mut self.clipboard
            {
                match prepare_osc52_copy(s, OSC52_MAX) {
                    Ok(text) => {
                        if let Err(error) = set_osc52_clipboard(cb, target, text) {
                            log::warn!(
                                "clipboard write failed (OSC 52 {target:?} target): {error}"
                            );
                        }
                    }
                    Err(error) => log::warn!("OSC 52: could not prepare text: {error}"),
                }
            }
        }
    }

    fn drain_events(&mut self, ws: &mut WindowState) {
        self.sync_session_log_failures(ws);
        let mut bell = false;
        // Pane ids that rang the bell this drain
        // pass — latched onto their containing tabs *after* the
        // iter_mut() loop so we don't double-borrow mux.panes.
        let mut bell_panes: Vec<u64> = Vec::new();
        // Pane ids + raw-output chunks accumulated during this drain pass.
        // Keep the PTY reader's bounded chunks intact instead of concatenating
        // a whole backlog into a second potentially huge allocation.
        let mut output_chunks: Vec<(u64, Vec<u8>)> = Vec::new();
        // Collect OSC-133 CommandFinished events per
        // pane so the App is the SINGLE drainer — `drain_command_finished_events`
        // is destructive, so the command-notify, the run_command correlator, and
        // event subscribers must all be fed from one place, AFTER this pane loop.
        let mut command_finished_local: Vec<(u64, kettle_core::CommandFinished)> = Vec::new();
        // Protocol desktop notifications (OSC 9 / OSC 777) drained once per
        // pass and dispatched after the pane loop so the UI owns all toasts.
        let mut protocol_notifications_local: Vec<(u64, kettle_core::ProtocolNotification)> =
            Vec::new();
        // Pane ids whose shell exited with cfg.exit_action
        // = Restart. Queued during the drain; appended to
        // ws.pending_pane_restarts after the iteration so the
        // post-drain handler can process them with a fresh borrow.
        let mut pending_restarts_local: Vec<u64> = Vec::new();
        for (&pane_id, pane) in ws.mux.panes.iter_mut() {
            if pane.pty_input_failed() {
                log::error!("closing pane {pane_id}: its PTY input transport failed");
                pane.closed = true;
                while pane.rx.try_recv().is_ok() {}
                continue;
            }
            // Read the exit latch before anything else: the reader sets it
            // after the last of the PTY's output, so everything that output
            // latched or queued is taken below, before exit policy applies.
            // An exit latched after this read waits for the next drain.
            let exited = pane.term.take_exited();
            // Drain the optional output sidechannel before regular events.
            if let Some(out_rx) = &pane.output_rx {
                while let Ok(chunk) = out_rx.try_recv() {
                    output_chunks.push((pane_id, chunk));
                }
            }
            // Bells are latched by the terminal, not queued with the events
            // below. Terminator parity (terminatorlib/config.py:103
            // `force_no_bell`): silence every bell flavor regardless of
            // cfg.bell mode.
            if pane.term.take_bell() && !self.cfg.force_no_bell {
                bell = true;
                bell_panes.push(pane_id);
            }
            // Titles, cursor-blink switches and OSC 52 writes are latched by
            // the terminal too, the newest kept, so a program repeating them
            // cannot fill the event queue. They are taken before the queue
            // below, so a program that writes the clipboard and then reads it
            // back sees its write.
            match pane.term.take_title() {
                Some(kettle_core::TitleChange::Set(t)) => {
                    // Ignore the bogus full-exe-path title conhost/
                    // ConPTY injects at startup for a native Windows shell —
                    // keeping the "kettle" placeholder so the tab/window/pane
                    // label falls back to the working directory (the OSC 7/9;9
                    // cwd, else the natively-polled cwd) instead of showing an
                    // ugly `…\pwsh.exe` path. Only a still-placeholder pane is
                    // checked; ANY genuine OSC 2 title (incl. a later one from
                    // the shell's own prompt) is stored and permanently ends
                    // suppression, so a program-set title is never lost.
                    if pane.title_is_placeholder
                        && suppress_startup_title(&t, &pane.argv, cfg!(windows))
                    {
                        // keep the placeholder seed; do not store the exe path
                    } else {
                        apply_shell_title(
                            &mut pane.title,
                            &mut pane.title_is_placeholder,
                            &mut pane.title_origin,
                            &mut pane.title_before_remote,
                            Some(&t),
                        );
                    }
                }
                Some(kettle_core::TitleChange::Reset) => {
                    apply_shell_title(
                        &mut pane.title,
                        &mut pane.title_is_placeholder,
                        &mut pane.title_origin,
                        &mut pane.title_before_remote,
                        None,
                    );
                }
                None => {}
            }
            if pane.term.take_cursor_blinking_changed() {
                // DEC mode 12 (`CSI ?12 h/l`) just flipped. The next
                // redraw picks up the new state from
                // `Terminal::cursor_blinking()`, but reset the blink
                // phase now so *blink-on* starts visible and
                // *blink-off* makes the cursor solid right away, not on
                // whatever half-period we'd otherwise land in. It is
                // program output, so it does not restart the blink
                // timeout; only typing, focus, and settings do.
                ws.blink_on = true;
                ws.last_blink = std::time::Instant::now();
            }
            self.store_osc52_writes(pane.term.take_clipboard_stores());
            while let Ok(ev) = pane.rx.try_recv() {
                match ev {
                    TermEvent::PtyWrite(s) => {
                        if pane.queue_protocol_reply(s.as_bytes()).is_failed() {
                            log::error!("closing pane {pane_id}: bounded PTY input queue failed");
                            pane.closed = true;
                            break;
                        }
                    }
                    TermEvent::ClipboardLoad(target, fmt) => {
                        // A write latched since the take above was made before
                        // this read: apply it first, so the program reads it.
                        self.store_osc52_writes(pane.term.take_clipboard_stores());
                        // OSC 52 read lets a (remote) program exfiltrate the
                        // clipboard; denied by default — reply empty so the
                        // protocol stays well-formed without leaking.
                        let text = if self.cfg.osc52.can_paste() {
                            self.clipboard
                                .as_mut()
                                .and_then(|c| match get_osc52_clipboard(c, target) {
                                    Ok(text) => Some(text),
                                    Err(error) => {
                                        log::warn!(
                                            "clipboard read failed (OSC 52 {target:?} target): \
                                             {error}"
                                        );
                                        None
                                    }
                                })
                                .unwrap_or_default()
                        } else {
                            String::new()
                        };
                        // Bound the reply symmetrically with the write path
                        // above: the OS clipboard is outside kettle's control
                        // (a big log/diff/webpage copy is plausible with no
                        // attacker involved, and a malicious remote program
                        // querying via OSC 52 `?` shouldn't be able to force an
                        // unbounded base64 encode + single PTY write either
                        // way).
                        let text = clamp_osc52(&text, OSC52_MAX);
                        let reply = fmt(text);
                        if pane.queue_protocol_reply(reply.as_bytes()).is_failed() {
                            log::error!("closing pane {pane_id}: bounded PTY input queue failed");
                            pane.closed = true;
                            break;
                        }
                    }
                    TermEvent::TextAreaSizeRequest(fmt) => {
                        // CSI 14 t — text-area size in pixels. Sixel / kitty
                        // graphics / iTerm2-OSC-1337 apps probe this to do
                        // pixel-perfect image placements. Use the exact totals
                        // already published through PtySize: at fractional DPI
                        // a 9.6 px cell across 100 columns is 960 px, not the
                        // 1000 px produced by multiplying a rounded 10 px cell.
                        let (pixel_width, pixel_height) = pane.term.pty_pixel_size();
                        let reply = kettle_render::reply_for_text_area_size(
                            pixel_width,
                            pixel_height,
                            &*fmt,
                        );
                        if pane.queue_protocol_reply(reply.as_bytes()).is_failed() {
                            log::error!("closing pane {pane_id}: bounded PTY input queue failed");
                            pane.closed = true;
                            break;
                        }
                    }
                    TermEvent::ColorRequest(idx, fmt) => {
                        // OSC 4 ; n ; ? / OSC 10 / 11 / 12 — resolve against
                        // the active theme + any runtime overrides, then let
                        // the engine-supplied formatter render the canonical
                        // xparsecolor reply and write it back to the PTY. No
                        // reply for out-of-range indices keeps the protocol
                        // well-formed (apps that probe just see a timeout,
                        // exactly as on terminals that don't support OSC).
                        let reply = pane.term.term.lock().ok().and_then(|t| {
                            kettle_render::reply_for_query(idx, &self.cfg.theme, t.colors(), &*fmt)
                        });
                        if let Some(s) = reply
                            && pane.queue_protocol_reply(s.as_bytes()).is_failed()
                        {
                            log::error!("closing pane {pane_id}: bounded PTY input queue failed");
                            pane.closed = true;
                            break;
                        }
                    }
                    TermEvent::ChildExit(_) => {
                        // Process status is advisory, not a pane-lifecycle
                        // boundary. alacritty_terminal can publish ChildExit
                        // before its final PTY drain; only the exit latch
                        // below may apply exit policy or make this pane
                        // reapable.
                    }
                    _ => {}
                }
            }
            // Terminator parity (terminatorlib/config.py:118
            // `exit_action`): when the shell exits, close the pane,
            // restart the shell, or hold the dead shell visible so the
            // user can read its final output and scrollback.
            //
            // Hold: don't mark closed; the pane shows its last output
            // until the user closes it (Ctrl+Shift+W).
            // Restart: mark closed and queue the pane id so this loop
            // never double-borrows ws.mux; the post-drain handler
            // respawns the same argv and cwd in a new tab.
            // Close (default): mark closed.
            //
            // `exited` was read before this pane's latches and queue, so its
            // final title, clipboard writes and replies were handled above.
            // `Mux::reap` keys off this instead of polling the process, so a
            // fast child cannot disappear before its final output or exit
            // policy is applied.
            if exited && !pane.closed {
                apply_pane_exit_action(
                    self.cfg.exit_action,
                    pane_id,
                    pane,
                    &mut pending_restarts_local,
                );
            }
            #[cfg(windows)]
            if !pane.exit_observed
                && let Some(child_exit_at) = pane.term.direct_child_exit_at()
            {
                let now = std::time::Instant::now();
                let phase = match pane.pty_output_close_phase {
                    PtyOutputClosePhase::NotStarted => BoundedPtyExitPhase::NotStarted,
                    PtyOutputClosePhase::InProgress(started) => {
                        BoundedPtyExitPhase::InProgress(now.saturating_duration_since(started))
                    }
                    PtyOutputClosePhase::Failed { retry_at } => BoundedPtyExitPhase::Failed {
                        retry_in: retry_at.saturating_duration_since(now),
                    },
                };
                match bounded_pty_exit_action(now.saturating_duration_since(child_exit_at), phase) {
                    BoundedPtyExitAction::Wait => {}
                    BoundedPtyExitAction::BeginClose => {
                        log::warn!(
                            "PTY output stayed open after pane {pane_id}'s direct child exited; \
                             starting bounded ConPTY close"
                        );
                        match pane.term.begin_pty_output_close() {
                            Ok(_) => {
                                pane.pty_output_close_phase =
                                    PtyOutputClosePhase::InProgress(std::time::Instant::now())
                            }
                            Err(error) => {
                                log::error!("cannot close pane {pane_id}'s ConPTY output: {error}");
                                pane.pty_output_close_phase = PtyOutputClosePhase::Failed {
                                    retry_at: std::time::Instant::now()
                                        + std::time::Duration::from_secs(1),
                                };
                            }
                        }
                    }
                    BoundedPtyExitAction::ApplyPolicy => {
                        // Older ConPTY builds may park ClosePseudoConsole while
                        // an inherited client remains attached. The close
                        // worker is detached specifically for that platform
                        // behavior; do not let it also park GUI exit policy.
                        log::error!(
                            "pane {pane_id}'s PTY did not finish within the bounded drain; final \
                             output may be incomplete"
                        );
                        apply_pane_exit_action(
                            self.cfg.exit_action,
                            pane_id,
                            pane,
                            &mut pending_restarts_local,
                        );
                    }
                }
            }
            if pane.pty_input_failed() {
                log::error!("closing pane {pane_id}: its PTY input transport failed");
                pane.closed = true;
                while pane.rx.try_recv().is_ok() {}
            }
            // Drain OSC 133 D (CommandEnd) events ONCE here and
            // defer processing to after the pane loop, where the App can fan a
            // single drain out to the command-notify, the run_command
            // correlator, and event subscribers (each consumer would otherwise
            // race for the destructive drain). Always drain (not gated on the
            // notify threshold) so a pending run_command still resolves when
            // notifications are off.
            for ev in pane.term.drain_command_finished_events() {
                command_finished_local.push((pane_id, ev));
            }
            for ev in pane.term.drain_protocol_notifications() {
                protocol_notifications_local.push((pane_id, ev));
            }
        }
        if bell {
            if self.cfg.bell.visual() {
                // Only the pane that rang flashes; a sibling split's bell
                // then tells you which one it was. Panes outside the current
                // layout keep the tab dot and titlebar glyph as their cue.
                let now = std::time::Instant::now();
                for id in &bell_panes {
                    ws.bell_flashes.insert(*id, now);
                }
            }
            if self.cfg.bell.attention()
                && !ws.window_focused
                && let Some(w) = &ws.window
            {
                w.request_user_attention(Some(UserAttentionType::Informational));
                ws.attention_active = true;
            }
        }
        // Latch any per-pane bells onto their tab's
        // activity flag so the tab-bar dot survives even on tabs the
        // user isn't currently looking at. Active-tab bells were
        // already handled visually (`bell_flashes` above drives the
        // per-pane flash); the latching helper skips the active
        // tab so we don't double-signal.
        //
        // Terminator plugin parity:
        // fire LuaEvent::Bell(pane_id) for every belled pane after
        // the kettle-side bell handling is done. Callbacks may queue
        // LuaCommands (kettle.notify, kettle.send_text); they get
        // transferred immediately into the bounded App FIFO by
        // `fire_lua_event`, then dispatched in order on the event loop.
        // `fire_lua_event` drains through `drain_lua_hook_commands`, the one
        // canonical LuaCommand-match path that every Lua hook shares.
        for id in bell_panes {
            ws.mux.touch_tab_bell(id);
            self.fire_lua_event(ws, crate::LuaEvent::Bell(id), "bell hook");
        }
        // The focused pane is being looked at, so its bell is answered.
        //
        // Deliberately here rather than on focus-change events: focus moves by
        // far more routes than those fire on — a tab switch, `reap` promoting a
        // sibling when the focused shell exits, the ctl API, a layout restore —
        // and every one of them would have to remember. This runs each drain
        // against the true current focus, so none of them can leave the latch
        // lit on a pane the user is staring at.
        ws.mux.clear_focused_pane_bell();
        // A single destructive drain fans out to Lua and the
        // optional recorder so neither consumer can steal bytes from the other.
        self.dispatch_output_chunks(ws, output_chunks);
        // Fan out the OSC-133 CommandFinished events
        // drained above — command-notify (existing behavior), the run_command
        // correlator, and event subscribers all from this single place.
        for (pane_id, ev) in command_finished_local {
            // (a) Desktop notification for a long background command.
            if self.cfg.command_notify_threshold_ms > 0 {
                let elapsed_ms = ev.duration.as_millis() as u64;
                if !ws.window_focused && elapsed_ms >= self.cfg.command_notify_threshold_ms {
                    let secs = ev.duration.as_secs();
                    let tr = &self.ui_text;
                    let exit_text = match ev.exit_code {
                        Some(0) => tr.text(kettle_i18n::Text::NotifyBodyCommandOk).to_string(),
                        Some(code) => tr.notify_body_command_exit(&code.to_string()),
                        None => String::new(),
                    };
                    let body = if exit_text.is_empty() {
                        tr.notify_body_command_ran(pane_id, secs)
                    } else {
                        tr.notify_body_command_result(pane_id, secs, &exit_text)
                    };
                    fire_notify(
                        tr.text(kettle_i18n::Text::NotifyTitleCommandFinished),
                        &body,
                    );
                }
            }
            // (b) Resolve a pending run_command for this pane.
            self.resolve_pending_run(ws, pane_id, &ev);
            // (c) Notify event subscribers.
            self.ctl_broadcast(
                "command_finished",
                Some(pane_id),
                serde_json::json!({
                    "exit_code": ev.exit_code,
                    "duration_ms": ev.duration.as_millis() as u64,
                }),
            );
        }
        for (pane_id, ev) in protocol_notifications_local {
            fire_notify(&ev.title, &ev.body);
            if !ws.window_focused
                && let Some(w) = &ws.window
            {
                w.request_user_attention(Some(UserAttentionType::Informational));
                ws.attention_active = true;
            }
            self.ctl_broadcast(
                "protocol_notification",
                Some(pane_id),
                serde_json::json!({
                    "title": ev.title,
                    "body": ev.body,
                }),
            );
        }
        // Stash the restart list on the window so the post-drain handler can
        // process it with a fresh &mut ws.mux borrow. The pane loop above holds a
        // &mut iterator into ws.mux.panes, so spawn_pane can't run there.
        if !pending_restarts_local.is_empty() {
            ws.pending_pane_restarts.extend(pending_restarts_local);
        }
    }

    fn process_pending_pane_restarts(&mut self, ws: &mut WindowState) {
        if ws.pending_pane_restarts.is_empty() {
            return;
        }
        let pane_ids: Vec<u64> = std::mem::take(&mut ws.pending_pane_restarts);
        let waker = self.waker();
        let (cols, rows) = self.grid_of(ws, self.area(ws));
        let geometry = self.pty_geometry_for_grid(ws, cols, rows);
        for pane_id in pane_ids {
            let restart_info: Option<(Vec<String>, Option<String>)> = ws
                .mux
                .panes
                .get(&pane_id)
                .map(|pane| (pane.argv.clone(), pane.term.current_dir_or_native()));
            if let Some((argv, cwd)) = restart_info {
                let pre = self.focus_key(ws);
                if let Err(error) = ws.mux.new_tab_with_launch_geometry(
                    &self.cfg,
                    geometry,
                    waker.clone(),
                    argv,
                    cwd,
                ) {
                    log::warn!("exit-action = restart: spawn failed for pane {pane_id}: {error}");
                } else {
                    self.note_focus_change(ws, pre);
                    self.fire_tab_add_event(ws);
                }
            }
        }
    }

    /// Fan out PTY output still queued in
    /// the shared output sidechannels right now. Output that lands after the
    /// last redraw drain and before a pane is reaped or its window closes would
    /// otherwise be lost to both the recorder and Lua hooks. Call this before
    /// `mux.reap()` and on close so both consumers keep their tail. Recorder
    /// events batch on the bounded persistence worker; clean close paths hand
    /// finalization to that worker.
    fn flush_recorder_output(&mut self, ws: &mut WindowState) {
        // Always drain what's queued right now (cheap, non-blocking).
        self.drain_recorder_output_once(ws);
        // A pane marked `closed` (its shell exited under exit-action = close /
        // restart) is about to be reaped + its sidechannel dropped. The PTY
        // reader may still be teeing the shell's FINAL output: the child-exit
        // signal can beat the reader's last write, so a sub-frame-lifetime
        // session (e.g. `-e cmd /c echo x`) would lose its output entirely.
        // Give the reader a brief BOUNDED window to finish, draining as we go.
        // Bounded to ~60 ms worst case and gated on a closed pane being present,
        // so steady-state frames (and exit-action = hold dead panes, which keep
        // `closed = false`) pay nothing. Early-out after 3 idle rounds.
        let mut idle = 0u8;
        let mut rounds = 0u8;
        while rounds < 30
            && ws
                .mux
                .panes
                .values()
                .any(|p| p.closed && p.output_rx.is_some())
        {
            std::thread::sleep(std::time::Duration::from_millis(2));
            if self.drain_recorder_output_once(ws) {
                idle = 0;
            } else {
                idle += 1;
                if idle >= 3 {
                    break;
                }
            }
            rounds += 1;
        }
    }

    /// Drain every pane's shared output sidechannel and fan each chunk out to
    /// the recorder and Lua once. Returns whether any bytes were captured.
    /// Keep this active after recorder startup failure: Lua may still be
    /// subscribed to the same destructive channel.
    fn drain_recorder_output_once(&mut self, ws: &mut WindowState) -> bool {
        // Collect first (immutable borrow of the panes), then dispatch through
        // `self` after that borrow ends.
        let mut tail: Vec<(u64, Vec<u8>)> = Vec::new();
        for (&pane_id, pane) in &ws.mux.panes {
            if let Some(rx) = &pane.output_rx {
                while let Ok(chunk) = rx.try_recv() {
                    tail.push((pane_id, chunk));
                }
            }
        }
        let got = !tail.is_empty();
        self.dispatch_output_chunks(ws, tail);
        got
    }

    fn recording_status(&self) -> Option<crate::dev_record::RecordStatus> {
        effective_record_status(
            self.recorder.as_ref().map(|rec| rec.status()),
            self.recording_start_failed,
        )
    }

    /// Reflect terminal recorder failures in both persistent window state and
    /// an edge-triggered desktop notification during the same event-loop turn.
    fn sync_recording_state(&mut self, ws: &mut WindowState, io_error_body: &str) {
        let status = self.recording_status();
        if matches!(
            status,
            Some(
                crate::dev_record::RecordStatus::Overloaded
                    | crate::dev_record::RecordStatus::IoError
            )
        ) && !self.recording_error_reported
        {
            self.recording_error_reported = true;
            let tr = &self.ui_text;
            let (title, body) = if status == Some(crate::dev_record::RecordStatus::Overloaded) {
                (
                    tr.text(kettle_i18n::Text::NotifyTitleRecordingStopped),
                    tr.text(kettle_i18n::Text::NotifyBodyRecordingOverloaded),
                )
            } else {
                (
                    tr.text(kettle_i18n::Text::NotifyTitleRecordingError),
                    io_error_body,
                )
            };
            fire_notify(title, body);
        }
        self.sync_window_title(ws);
    }

    /// Keep the OS window title in sync with the *active* pane's title —
    /// including after tab/focus switches, not only on OSC title events.
    /// Deduped so it isn't a syscall every frame.
    fn sync_window_title(&mut self, ws: &mut WindowState) {
        let want = self.desired_window_title(ws);
        if want != ws.last_title {
            if let Some(w) = &ws.window {
                w.set_title(&want);
            }
            ws.last_title = want;
        }
    }

    fn desired_window_title(&self, ws: &WindowState) -> String {
        let pane = ws.mux.active_focus().and_then(|id| ws.mux.panes.get(&id));
        let title = pane.map(|p| p.title.as_str()).unwrap_or("kettle");
        // OSC 7/9;9 cwd if the shell reported one, else the natively
        // polled cwd — so the OS window title tracks the directory for a stock
        // pwsh/cmd just like the tab label does.
        let cwd = pane
            .and_then(|p| p.term.current_dir_or_native())
            .unwrap_or_default();
        let tab = ws.mux.active + 1;
        let want = match &ws.window_title_override {
            Some(forced) => sanitize_title(forced),
            None => window_title(&self.cfg.window_title_format, title, &cwd, tab),
        };
        // An always-visible recording indicator in the title bar so the
        // recorder is never silently capturing (and `[REC RAW]` when raw
        // keystroke capture is on). Keep it ASCII because Linux window-manager
        // title fonts do not reliably carry symbol glyphs.
        let raw = self.recorder.as_ref().is_some_and(|rec| rec.raw_input());
        let want = recording_window_title(want, self.recording_status(), raw);
        // New media on the focused pane's shelf, in ASCII like the
        // recording mark, until the user opens it.
        let unseen = pane.map_or(0, |pane| pane.media_shelf.unseen());
        let want = if unseen > 0 {
            format!(
                "{} {want}",
                self.ui_text.window_title_new_media(unseen as u64)
            )
        } else {
            want
        };
        let want = sanitize_native_window_title(&want);
        if self.gpu_software_fallback {
            format!(
                "{want} - {}",
                self.ui_text
                    .text(kettle_i18n::Text::WindowTitleSoftwareFallback)
            )
        } else {
            want
        }
    }

    /// Visible terminal cursor cell for IME composition/candidate placement.
    fn terminal_cursor_cell(&self, ws: &WindowState) -> Option<(usize, usize)> {
        let pane = ws.mux.active_focus().and_then(|id| ws.mux.panes.get(&id))?;
        let term = pane.term.term.lock().ok()?;
        let point = term.renderable_content().cursor.point;
        Some((point.line.0.max(0) as usize, point.column.0))
    }

    fn update_ime_cursor_area(&self, ws: &WindowState) {
        let Some(window) = &ws.window else {
            return;
        };
        let (cw, ch) = ws
            .renderer
            .as_ref()
            .map(|renderer| (renderer.cell_w, renderer.cell_h))
            .unwrap_or((8.0, 16.0));
        let (preedit, preedit_focus) = ws
            .ime_preedit
            .as_ref()
            .map(|(text, selection)| (text.as_str(), ime_preedit_focus(text, *selection)))
            .unwrap_or(("", 0));
        let preedit_before_focus = &preedit[..preedit_focus];
        let input = ws
            .palette_input
            .as_ref()
            .map(|(query, _)| (4, query.as_str()))
            .or_else(|| {
                ws.theme_picker_input.as_ref().map(|state| {
                    use unicode_width::UnicodeWidthStr as _;
                    (
                        kettle_render::theme_picker_prefix(&self.ui_text).width(),
                        state.query.as_str(),
                    )
                })
            })
            .or_else(|| {
                ws.layout_picker_input
                    .as_ref()
                    .map(|(query, _)| (12, query.as_str()))
            })
            .or_else(|| ws.ssh_input.as_ref().map(|(query, _)| (8, query.as_str())));
        let search_cursor = self.search_geometry(ws).map(|geometry| {
            let mut projected = ws.search.query().to_string();
            let replace = ws
                .search
                .editor
                .selection()
                .unwrap_or(ws.search.editor.cursor()..ws.search.editor.cursor());
            projected.replace_range(replace.clone(), preedit);
            let cursor_byte = replace.start.saturating_add(preedit_focus);
            let columns = kettle_render::search_bar_columns(geometry.editor.2, cw).max(1);
            let scroll = crate::search_input::SearchEditor::visible_scroll_for(
                &projected,
                cursor_byte,
                ws.search.editor.horizontal_scroll(),
                columns.saturating_sub(2),
            );
            let cursor_column = crate::search_input::display_width(&projected[..cursor_byte])
                .saturating_sub(scroll);
            let left = geometry.editor.0 + cw;
            let right = (geometry.editor.0 + geometry.editor.2 - cw).max(left);
            // The committed text before the composition is placed where it
            // was painted, so a CJK fallback glyph wider or narrower than two
            // cells does not drift the candidate window; the composition
            // itself counts cells.
            let stops = ws
                .renderer
                .as_ref()
                .map_or(&[][..], |renderer| renderer.search_editor_stops());
            let painted = stops
                .binary_search_by_key(&replace.start, |stop| stop.0)
                .ok()
                .map(|index| stops[index].1 + preedit_before_focus.width() as f32 * cw);
            (
                painted
                    .unwrap_or(geometry.editor.0 + (cursor_column + 1) as f32 * cw)
                    .clamp(left, right),
                geometry.editor.1 + ((geometry.editor.3 - ch) * 0.5).max(0.0),
            )
        });
        let (x, y) = if let Some((x, y)) = search_cursor {
            (x, y)
        } else if let Some((prefix_cols, input)) = input {
            use unicode_width::UnicodeWidthStr as _;

            let size = window.inner_size();
            let cols = prefix_cols + input.width() + preedit_before_focus.width();
            (
                (cols as f32 * cw).min(size.width.saturating_sub(1) as f32),
                (size.height as f32 - ch - 5.0).max(0.0),
            )
        } else if let Some(edit) = &ws.editing_title {
            let rect = self.title_edit_rect(ws);
            let caret =
                title_edit_caret_cols(edit.scope, &edit.input, preedit_before_focus, &self.ui_text);
            (
                rect.0 + caret as f32 * cw,
                rect.1 + ((rect.3 - ch) * 0.5).max(0.0),
            )
        } else if ws.settings_text_edit.is_some() {
            let size = window.inner_size();
            (size.width as f32 * 0.5, size.height as f32 * 0.5)
        } else {
            let Some((row, col)) = self.terminal_cursor_cell(ws) else {
                return;
            };
            let Some(rect) = self.focused_terminal(ws, self.area(ws)) else {
                return;
            };
            let grid_origin = (rect.0 + self.cfg.padding_x, rect.1 + self.cfg.padding_y);
            (
                grid_origin.0 + col as f32 * cw,
                grid_origin.1 + row as f32 * ch,
            )
        };
        let size = window.inner_size();
        let x = x.clamp(0.0, size.width.saturating_sub(1) as f32);
        let y = y.clamp(0.0, size.height.saturating_sub(1) as f32);
        window.set_ime_cursor_area(
            winit::dpi::PhysicalPosition::new(f64::from(x), f64::from(y)),
            winit::dpi::PhysicalSize::new(f64::from(cw.max(1.0)), f64::from(ch.max(1.0))),
        );
    }

    /// `(row, col)` of the mouse within `rect`.
    ///
    /// Clamped to the focused pane's live grid: a click in the right/bottom
    /// padding (the rect rounds up past an exact cell multiple) must report the
    /// LAST cell, never one past the edge. A mouse-tracking app that sees
    /// `col == cols` or `row == rows` mis-renders — xterm
    /// itself clamps the reported coordinate to the window.
    fn cursor_cell_in_rect(&self, ws: &WindowState, rect: Rect) -> (usize, usize) {
        // Mouse-tracking reports a cell, not a selection boundary — the sub-cell
        // side is irrelevant here, so discard it.
        let (p, _) = self.px_to_point(ws, rect, ws.cursor.x as f32, ws.cursor.y as f32);
        let (row, col) = (p.line.0.max(0) as usize, p.column.0);
        // Clamp to the terminal's grid, the size `resize_all` gave the pane's
        // PTY.
        let (cols, rows) = self.grid_of(ws, rect);
        (
            row.min(rows.saturating_sub(1)),
            col.min(cols.saturating_sub(1)),
        )
    }

    /// `(row, col)` of the mouse within the focused pane, if any.
    fn cursor_cell(&self, ws: &WindowState) -> Option<(usize, usize)> {
        let rect = self.focused_terminal(ws, self.area(ws))?;
        Some(self.cursor_cell_in_rect(ws, rect))
    }

    fn link_at_cursor<'a>(&self, ws: &'a WindowState) -> Option<&'a kettle_core::Link> {
        let rect = self.focused_terminal(ws, self.area(ws))?;
        if !rect_contains(rect, ws.cursor.x as f32, ws.cursor.y as f32) {
            return None;
        }
        let (row, col) = self.cursor_cell_in_rect(ws, rect);
        ws.links
            .iter()
            .find(|l| l.row == row && col >= l.start_col && col <= l.end_col)
    }

    fn pane_mode(&self, ws: &WindowState, pane_id: u64) -> Option<kettle_core::TermMode> {
        ws.mux
            .panes
            .get(&pane_id)
            .and_then(|p| p.term.term.lock().ok().map(|t| *t.mode()))
    }

    fn completion_overlay_projection(
        &self,
        ws: &WindowState,
        terminal_surface: bool,
    ) -> Option<kettle_render::CompletionOverlay> {
        if !terminal_surface || !ws.window_focused || ws.ime_preedit.is_some() {
            return None;
        }
        let pane_id = ws.mux.active_focus()?;
        let pane = ws.mux.panes.get(&pane_id)?;
        if !pane.completion_overlay {
            return None;
        }
        let (mode, display_offset, columns, rows) = {
            let term = pane.term.term.lock().ok()?;
            (
                *term.mode(),
                term.grid().display_offset(),
                term.columns(),
                term.screen_lines(),
            )
        };
        if !completion_surface_available(mode, display_offset) {
            return None;
        }
        let command_rows = pane.term.completion_command_rows()?;
        let (list, completion_cursor_col) = pane.term.completion_projection()?;
        let anchor_col = completion_cursor_col
            .zip(list.input_prefix.as_deref())
            .map(|(cursor, prefix)| completion_anchor_col(cursor, prefix));
        let area = self.area(ws);
        let pane_rect = self.focused_terminal(ws, area)?;
        let grid_origin = (
            pane_rect.0 + self.cfg.padding_x,
            pane_rect.1 + self.cfg.padding_y,
        );
        let renderer = ws.renderer.as_ref()?;
        let kind = self.ui_text.text(match list.kind {
            kettle_core::CompletionKind::Completion => kettle_i18n::Text::CompletionKindCompletions,
            kettle_core::CompletionKind::Prediction => kettle_i18n::Text::CompletionKindPrediction,
        });
        Some(kettle_render::CompletionOverlay {
            tr: self.ui_text,
            pane_rect,
            grid_rect: (
                grid_origin.0,
                grid_origin.1,
                columns as f32 * renderer.cell_w,
                rows as f32 * renderer.cell_h,
            ),
            command_rows,
            anchor_col,
            kind: kind.to_string(),
            source: list.source,
            selected: list.selected,
            total: list.total,
            token: list.token,
            candidates: list
                .candidates
                .into_iter()
                .map(|candidate| kettle_render::CompletionOverlayRow {
                    label: candidate.label,
                    description: candidate.description,
                    position: candidate.position,
                })
                .collect(),
        })
    }

    fn overlay(&self, ws: &WindowState, reuse_pane_snapshots: bool) -> Overlay {
        let preedit_state = ws.ime_preedit.as_ref().filter(|(text, _)| !text.is_empty());
        let preedit = preedit_state.map(|(text, _)| text.as_str());
        let with_preedit = |input: &str| match preedit {
            Some(preedit) => format!("{input}{preedit}"),
            None => input.to_string(),
        };
        // Hovering any segment of a link wrapped across rows lights them all.
        let hovered_group = self.link_at_cursor(ws).map(|link| link.group);
        let links = ws
            .links
            .iter()
            .map(|l| kettle_render::LinkRect {
                col: l.start_col,
                row: l.row,
                width: (l.end_col + 1).saturating_sub(l.start_col).max(1),
                hover: hovered_group == Some(l.group),
            })
            .collect();

        use kettle_i18n::Text as T;
        let tr = &self.ui_text;
        let (ssh_query, ssh_hint, ssh_picker) = match &ws.ssh_input {
            Some((query, selected)) => (
                Some(with_preedit(query)),
                picker_hint(
                    tr,
                    &[
                        ("Enter", T::PickerHintConnect),
                        ("Tab", T::PickerHintComplete),
                        ("↑↓", T::PickerHintSelect),
                        ("Esc", T::PickerHintCancel),
                    ],
                ),
                Some(ssh_picker_list(query, *selected, &self.cfg.ssh_hosts, tr)),
            ),
            None => (None, String::new(), None),
        };

        let (palette_query, palette_hint, palette_picker) = match &ws.palette_input {
            Some((query, selected)) => (
                Some(with_preedit(query)),
                picker_hint(
                    tr,
                    &[
                        ("Enter", T::PickerHintRun),
                        ("Tab/↑↓", T::PickerHintSelect),
                        ("Esc", T::PickerHintCancel),
                    ],
                ),
                Some(command_picker_list(
                    query,
                    *selected,
                    &self.cfg.keybinds,
                    tr,
                    self.update_notice().as_ref(),
                )),
            ),
            None => (None, String::new(), None),
        };

        // Terminator parity (layoutlauncher.py): build the layout picker's
        // query and hint the same way as the command palette. An empty
        // layouts dir is fine; the picker shows a "No saved layouts" row
        // hinting `kettle --layout NAME`.
        let (layout_picker_query, layout_picker_hint, layout_picker) = match &ws.layout_picker_input
        {
            Some((query, selected)) => (
                Some(with_preedit(query)),
                picker_hint(
                    tr,
                    &[
                        ("Enter", T::PickerHintSpawn),
                        ("Tab/↑↓", T::PickerHintSelect),
                        ("Esc", T::PickerHintCancel),
                    ],
                ),
                Some(layout_picker_list(
                    query,
                    *selected,
                    &ws.layout_picker_entries,
                    tr,
                )),
            ),
            None => (None, String::new(), None),
        };
        let (theme_picker_query, theme_picker_hint, theme_picker) = match &ws.theme_picker_input {
            Some(state) => (
                Some(with_preedit(&state.query)),
                picker_hint(
                    tr,
                    &[
                        ("Enter", T::PickerHintKeep),
                        ("↑↓", T::PickerHintSelect),
                        ("Esc", T::PickerHintRestore),
                    ],
                ),
                Some(theme_picker_list(
                    &state.query,
                    state.selected,
                    &state.opened_on,
                    tr,
                )),
            ),
            None => (None, String::new(), None),
        };
        let hint_labels: Vec<HintLabel> = match &ws.hint_state {
            Some((targets, typed)) => targets
                .iter()
                .map(|t| HintLabel {
                    row: t.row,
                    col: t.col,
                    label: t.label.clone(),
                    dim: !typed.is_empty() && !t.label.starts_with(typed.as_str()),
                })
                .collect(),
            None => Vec::new(),
        };
        let terminal_surface = terminal_surface_available(ws);
        let completion = self.completion_overlay_projection(ws, terminal_surface);
        let media_paste_receipt = self.media_paste_receipt_overlay_projection(
            ws,
            terminal_surface,
            std::time::Instant::now(),
        );
        // Lanes stay drawn under every modal, which paints over them; their
        // room in the pane stays carved either way.
        let media_lanes = self.preview_lane_projections(ws);
        let media_viewer_rect = ws.preview_overlay.and_then(|open| {
            media_lanes
                .iter()
                .find(|lane| lane.pane == open.pane)
                .map(|lane| lane.rect)
        });
        let ime_preedit = preedit.filter(|_| terminal_surface).and_then(|text| {
            let (row, col) = self.terminal_cursor_cell(ws)?;
            Some(ImePreedit {
                text: text.to_string(),
                row,
                col,
            })
        });

        let window_focused = ws.window_focused;
        // Brighten the focused pane's scrollbar while the pointer is on the
        // gutter or the thumb is being dragged (overlay-scrollbar feel). The
        // scrolled-back case is decided per-pane in the renderer from the snapshot.
        let scrollbar_active = ws.scrollbar_hover || ws.scrollbar_drag_offset.is_some();
        // Cursor blink is the *intersection* of the user config and the
        // running app's wishes — programs flip it via DEC private mode 12
        // (`CSI ?12 h/l`), which the engine tracks per-pane on its
        // `cursor_style().blinking`. Read the active pane's live state so
        // editors like vim that disable blink for their own cursor are
        // honored even when the global config wants blink. (Goes through
        // `active_focus` + `panes.get` so the `overlay()` builder stays a
        // pure `&self` reader.)
        let pane_blink = self.pane_cursor_blinking(ws, reuse_pane_snapshots);
        let blink_enabled = self.cfg.cursor_blink && pane_blink;
        let cursor_visible = if !blink_enabled || !window_focused || modal_holds_cursor_steady(ws) {
            true
        } else {
            ws.blink_on
        };
        // The transient resize chip. `about_to_wait` drives the expiry
        // repaint and clears the state.
        let resize_overlay = ws
            .resize_overlay
            .and_then(|(c, r, t)| (t.elapsed() < RESIZE_OVERLAY_DURATION).then_some((c, r)));

        let context_menu = self.context_menu_overlay(ws).or_else(|| {
            let surface = ws
                .renderer
                .as_ref()
                .map(|renderer| {
                    let (width, height) = renderer.surface_size();
                    (width as f32, height as f32)
                })
                .unwrap_or((800.0, 600.0));
            picker_overlay_context_menu(
                [palette_picker, theme_picker, layout_picker, ssh_picker],
                surface,
                self.menu_cell(ws),
            )
        });
        // Marshal the in-progress Edit-title state for
        // the render layer so the user sees what they're typing. The
        // title-edit rect is app chrome, never a bottom overlay over terminal
        // rows.
        let edit_title: Option<kettle_render::TitleEditOverlay> =
            ws.editing_title.as_ref().map(|s| {
                let label = title_edit_label(s.scope, &self.ui_text);
                kettle_render::TitleEditOverlay {
                    label: label.to_string(),
                    input: with_preedit(&s.input),
                    hint: picker_hint(
                        &self.ui_text,
                        &[
                            ("Enter", kettle_i18n::Text::EditHintApply),
                            ("Esc", kettle_i18n::Text::PickerHintCancel),
                        ],
                    ),
                    rect: self.title_edit_rect(ws),
                }
            });
        // Project the App's confirm_dialog into the
        // renderer's projection (so it shows even when no
        // search is open — confirm modals are independent).
        let confirm_dialog_early = ws.confirm_dialog.as_ref().map(|dialog| {
            let mut overlay = confirm_dialog_overlay(dialog, tr);
            if let Some(layout) = self.confirmation_layout(ws) {
                let (x, y) = (ws.cursor.x as f32, ws.cursor.y as f32);
                overlay.hover_idx = ws
                    .native_pointer
                    .and_then(|_| confirm_dialog_button_hit(&layout, x, y));
                if overlay.pressed_idx.is_none() {
                    overlay.pressed_idx = confirm_release_target(dialog.armed_press, &layout, x, y);
                }
            }
            overlay
        });
        // Project the settings overlay (independent of search, like
        // the confirm dialog). Values are read from the live Config so the
        // panel reflects the current state (incl. external reloads).
        let settings_overlay = self.settings_overlay_projection(ws);
        // Terminator parity: only a LIVE drag with a latched target paints a
        // hint. An armed-but-unmoved press is still a click, and a live drag
        // over a seam has no target to preview.
        let pane_drop_hint = ws
            .pane_drag
            .as_ref()
            .filter(|drag| drag.live)
            .and_then(|drag| drag.target)
            .map(|(_, rect, dir, before)| crate::mux::pane_drop_preview(rect, dir, before));
        let card_hover = ws.card_hover;
        let s = &ws.search;
        // Both literals list every field: a defaulted overlay would paint
        // English and drop state such as the pane drop hint.
        if !s.open {
            return Overlay {
                tr: self.ui_text,
                search: None,
                highlights: Vec::new(),
                links,
                ssh_query,
                ssh_hint,
                palette_query,
                palette_hint,
                layout_picker_query,
                layout_picker_hint,
                theme_picker_query,
                theme_picker_hint,
                edit_title,
                hint_labels,
                ime_preedit,
                completion,
                media_paste_receipt,
                media_lanes,
                media_viewer_rect,
                media_pointer: (!self.non_search_modal_open(ws))
                    .then_some(ws.native_pointer)
                    .flatten()
                    .map(|p| (p.x as f32, p.y as f32)),
                media_scrubbing: ws.lane_drag.is_some_and(|drag| drag.scrub),
                card_hover,
                window_focused,
                scrollbar_active,
                cursor_visible,
                resize_overlay,
                update_toast: self.update_toast_projection(ws),
                context_menu,
                confirm_dialog: confirm_dialog_early,
                settings: settings_overlay,
                pane_drop_hint,
            };
        }
        let mut highlights = Vec::new();
        if let Some(pane_id) = s.target_pane
            && let Some(pane) = ws.mux.panes.get(&pane_id)
            && let Ok(term) = pane.term.term.lock()
        {
            use kettle_core::Dimensions as _;
            let display_offset = term.grid().display_offset();
            let screen_lines = term.screen_lines();
            let last_column = term.last_column().0;
            let offset = i32::try_from(display_offset).unwrap_or(i32::MAX);
            let visible_top = -offset;
            let visible_bottom = i32::try_from(screen_lines.saturating_sub(1))
                .unwrap_or(i32::MAX)
                .saturating_sub(offset);
            for span in &s.visible {
                let first_line = span.start.line.max(visible_top);
                let last_line = span.end.line.min(visible_bottom);
                if first_line > last_line {
                    continue;
                }
                let active = s.focused == Some(*span);
                for line in first_line..=last_line {
                    let start_col = if line == span.start.line {
                        span.start.column
                    } else {
                        0
                    };
                    let end_col = if line == span.end.line {
                        span.end.column.min(last_column)
                    } else {
                        last_column
                    };
                    if end_col < start_col {
                        continue;
                    }
                    if let Some(rect) = HighlightRect::from_grid_span(
                        line,
                        display_offset,
                        screen_lines,
                        start_col,
                        end_col.saturating_sub(start_col).saturating_add(1),
                        active,
                    ) {
                        highlights.push(rect);
                    }
                }
            }
        }
        let confirm_dialog = confirm_dialog_early;
        let mut search_query = s.query().to_string();
        let mut search_cursor = s.editor.cursor();
        let mut search_selection = s.editor.selection().map(|range| (range.start, range.end));
        if let Some((preedit, selection)) = preedit_state {
            let replace = s.editor.selection().unwrap_or(search_cursor..search_cursor);
            search_query.replace_range(replace.clone(), preedit);
            search_cursor = replace
                .start
                .saturating_add(ime_preedit_focus(preedit, *selection));
            search_selection = None;
        }
        let search_horizontal_scroll = self
            .search_geometry(ws)
            .and_then(|geometry| {
                let renderer = ws.renderer.as_ref()?;
                let columns =
                    kettle_render::search_bar_columns(geometry.editor.2, renderer.cell_w).max(1);
                Some(crate::search_input::SearchEditor::visible_scroll_for(
                    &search_query,
                    search_cursor,
                    s.editor.horizontal_scroll(),
                    columns.saturating_sub(2),
                ))
            })
            .unwrap_or_else(|| s.editor.horizontal_scroll());
        Overlay {
            tr: self.ui_text,
            search: Some(kettle_render::SearchOverlay {
                tr: self.ui_text,
                target_pane: s.target_pane,
                query: search_query,
                cursor_byte: search_cursor,
                selection: search_selection,
                horizontal_scroll: search_horizontal_scroll,
                wrap: s.wrap,
                case_mode: map_search_case_mode(s.case_mode),
                invert: s.invert,
                status: displayed_search_status(s, std::time::Instant::now()),
                focused: s.focused_control,
                hovered: s.hovered_control,
                pressed: s.pressed_control,
            }),
            highlights,
            links,
            ssh_query,
            ssh_hint,
            palette_query,
            palette_hint,
            layout_picker_query,
            layout_picker_hint,
            theme_picker_query,
            theme_picker_hint,
            edit_title,
            hint_labels,
            ime_preedit,
            completion: None,
            media_paste_receipt: None,
            media_lanes,
            media_viewer_rect: None,
            media_pointer: None,
            media_scrubbing: false,
            card_hover,
            window_focused,
            scrollbar_active,
            cursor_visible,
            resize_overlay,
            update_toast: None,
            context_menu,
            confirm_dialog,
            settings: settings_overlay,
            pane_drop_hint,
        }
    }

    fn update_links(&mut self, ws: &mut WindowState) {
        scan_links(ws);
    }

    fn redraw(&mut self, ws: &mut WindowState) {
        // A frame while the window server blinks the cursor ends that blink.
        // Bring the app's phase to where the animation is first, so the frame
        // draws what the screen shows. The harness timer includes this work.
        let cursor_exit_frame = if ws.cursor_exit_log.enabled {
            ws.cursor_exit_log.begin_frame(
                ws.blink_layer.is_active(),
                crate::cursor_exit_log::raw_now_ns(),
            )
        } else {
            None
        };
        Self::materialize_layer_blink(ws, std::time::Instant::now());
        // Flush any pending chrome-geometry change before painting. A title
        // edit materialises a chrome strip (see `tab_bar_h`), so opening or
        // closing one changes the content rectangle and the PTYs must follow.
        // Doing it here rather than at each transition coalesces the
        // close-then-open pair that every modal opener performs: the child sees
        // one resize to the final geometry, or none at all when the geometry is
        // unchanged end to end, because `try_resize_geometry` already skips a
        // no-op.
        // Consume the flag only when the resize can happen. `resize_all`
        // returns early while the renderer is absent (GPU recovery), so
        // taking the flag then would drop the geometry change and the PTYs
        // would never hear of it. Leaving it set defers the resize to the
        // first frame after the renderer is rebuilt.
        if ws.pending_resize && ws.renderer.is_some() {
            ws.pending_resize = false;
            self.resize_all(ws);
        }
        // Keep the OS titlebar in step with the active palette.
        // Compare-only when nothing changed; independent of GPU health, so it
        // runs before the device-lost early-return below.
        self.maybe_sync_native_theme(ws);
        self.maybe_sync_color_scheme(ws);
        if let (Some(material), Some(window)) = (&ws.native_material, &ws.window) {
            material.sync(window, &self.cfg);
        }
        // If the GPU device was lost (a driver TDR/reset) or hit an uncaptured
        // error (VRAM exhaustion under memory pressure), rendering against it
        // cannot succeed. The wgpu error handlers installed by
        // `kettle_render::install_gpu_error_handlers` turn the fault into a
        // logged event and this flag instead of a `panic=abort` crash. Stop
        // painting so the surface-reconfigure loop does not spin;
        // about_to_wait_inner drives renderer recovery on a bounded backoff.
        if self.gpu.as_ref().is_some_and(|g| g.is_lost()) {
            let title = self.ui_text.text(kettle_i18n::Text::WindowTitleGpuLost);
            if ws.last_title != title {
                if let Some(w) = &ws.window {
                    w.set_title(title);
                }
                ws.last_title = title.to_string();
            }
            // Nothing is painted on the dead device, but pending PTY output
            // must still be marked seen and the output pacer reset. Otherwise
            // `window_has_new_output` stays true forever and a streaming pane
            // spins the event loop at 30-60 Hz during a TDR, burning CPU and
            // battery. With both cleared, the loop quiesces to
            // ControlFlow::Wait between output bursts. (`about_to_wait_inner`
            // also short-circuits animation wakes while lost.)
            ws.output_pacer.reset();
            ws.seen_output_gen.clear();
            for (id, p) in &ws.mux.panes {
                ws.seen_output_gen.insert(*id, p.term.output_generation());
            }
            return;
        }
        // Scheduled theme transitions are time-driven rather than terminal
        // output. Keep this lock-free maintenance outside the snapshot-reuse
        // branch so a stream of menu-hover frames cannot defer a boundary
        // crossing indefinitely.
        self.poll_theme_schedule(ws);
        // B (Peacock): resolve/refresh this window's accent claim (cheap in
        // the steady state; full pool walk only on first frame/theme switch).
        self.sync_window_accent(ws);
        // A context-menu hover changes only one highlight quad. When no output,
        // pane order, or grid dimensions changed, keep the prior render
        // snapshots and bypass every terminal-locking maintenance pass below.
        // The one-shot hint is invalidated by any intervening input/user event.
        let reuse_pane_snapshots = std::mem::take(&mut ws.reuse_pane_snapshots_once)
            && ws.context_menu.is_some()
            && context_menu_snapshot_reuse_safe(ws)
            && self.torn_drag.is_none()
            && self.pane_snapshots_match_current_layout(ws);
        if !reuse_pane_snapshots {
            self.drain_events(ws);
            self.poll_remote_contexts(ws);
            self.poll_focus_event(ws);
            self.poll_title_event(ws);
            if ws.ime_preedit.is_some() {
                self.update_ime_cursor_area(ws);
            }
            // Reflect the focused pane's OSC 9;4 progress onto the OS
            // taskbar button (pwsh 7 / Windows Terminal parity). No-op off Windows.
            self.poll_taskbar_progress(ws);
            // Process restart requests only after the pane iteration releases
            // its mutable borrow. Lifecycle-only wakes use this same helper in
            // about-to-wait, so hidden windows do not strand a Restart action.
            self.process_pending_pane_restarts(ws);
            // Reflect the active pane (incl. after tab/focus switches).
            self.sync_window_title(ws);
            // Capture a just-exited pane's final output before reap drops
            // its sidechannel — otherwise the shell's last line is lost from the trace.
            self.flush_recorder_output(ws);
            let reaped = ws.mux.reap();
            if reaped.layout_changed {
                ws.dragging_split = None;
            }
            self.fire_closed_pane_events(ws);
            // `clear_stale_title_edit` mutates `ws.editing_title`, so it is
            // evaluated into a binding rather than placed on the right of the
            // `||` where a reaped layout would short-circuit past it and
            // strand a title edit pinned to a pane that no longer exists.
            let stale_title = clear_stale_title_edit(&ws.mux, &mut ws.editing_title);
            if reaped.layout_changed || stale_title {
                ws.pending_resize = true;
                if let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
            self.hoover_groups(ws);
            if reaped.mux_empty {
                return;
            }
            ws.search_queries
                .retain(|pane_id, _| ws.mux.panes.contains_key(pane_id));
            // Track output without locking every terminal on every blink or
            // UI-only frame. The reader publishes `output_generation` after its
            // terminal mutation, so a changed generation is cheap and, unlike
            // `history_size()`, also catches in-place and alternate-screen
            // output. Only the opt-in scroll-on-output action takes a grid lock.
            let want_sob = self.cfg.scroll_on_output;
            // Track which panes produced output this frame so
            // we can latch their tab's `last_output_at`. Collected here
            // and dispatched after the borrow ends — same shape as the
            // `bell_panes` collection in `drain_events`.
            let mut output_panes: Vec<u64> = Vec::new();
            for (&pane_id, pane) in ws.mux.panes.iter_mut() {
                let generation = pane.term.output_generation();
                let advanced = output_generation_advanced(pane.last_output_generation, generation);
                if advanced {
                    output_panes.push(pane_id);
                }
                if want_sob
                    && advanced
                    && let Ok(mut t) = pane.term.term.lock()
                {
                    t.scroll_display(Scroll::Bottom);
                }
                pane.last_output_generation = Some(generation);
            }
            for id in output_panes {
                ws.mux.touch_tab_output(id);
            }
            // Auto-scroll while a selection stays at or beyond the focused
            // pane's top or bottom edge.
            // Pure `selection_autoscroll_lines` chooses the per-frame rate;
            // scrolling the viewport here naturally re-fires `update_selection`
            // below to anchor the selection's end to the new visible line.
            if ws.selecting {
                let area = self.area(ws);
                if let Some(pane_id) = ws.selecting_pane
                    && let Some(rect) = self.pane_terminal(ws, area, pane_id)
                {
                    let scale = ws
                        .window
                        .as_ref()
                        .map_or(1.0, |window| window.scale_factor() as f32);
                    let lines = if ws.selection_autoscroll_edge != 0 {
                        i32::from(ws.selection_autoscroll_edge)
                    } else {
                        selection_autoscroll_lines(
                            ws.cursor.y as f32,
                            rect.1,
                            rect.1 + rect.3,
                            scale,
                            ws.selection_dragged,
                        )
                    };
                    if lines != 0
                        && let Some(p) = ws.mux.panes.get(&pane_id)
                        && let Ok(mut t) = p.term.term.lock()
                    {
                        t.scroll_display(Scroll::Delta(lines));
                    }
                    if lines != 0 {
                        // Re-anchor the selection's end at the (now-moved)
                        // cursor row so the highlight grows in step with the
                        // scroll, not stuck on the original click-time row.
                        self.update_selection(ws, area);
                    }
                } else {
                    // The owner may still exist in an inactive tab. Preserve
                    // copy-on-select before invalidating the gesture.
                    self.finish_selection_gesture(ws);
                }
            }
            self.update_search(ws);
            self.update_links(ws);
            // Link set may have changed (scroll, output, mode flip) — re-sync
            // the cursor icon so a URL scrolling out from under a held Ctrl
            // doesn't leave the pointer-hand icon stuck on a now-empty cell.
            // Deduped via `last_cursor_icon` so this is a cheap per-frame
            // recheck when nothing changed.
            self.sync_cursor_icon(ws);
        }
        let overlay = self.overlay(ws, reuse_pane_snapshots);
        // Search and completion state are semantic accessibility state. Reuse
        // the exact projection this frame will draw instead of rebuilding the
        // full overlay twice for the accessibility key and tree. Snapshot reuse
        // skips pane maintenance, not semantic publication: a pending update
        // must not be stranded by a run of context-menu hover frames.
        self.publish_accessibility_if_due(ws, &overlay);
        let area = self.area(ws);
        let tabbar = self.tab_bar(ws);
        // Build status bar BEFORE the &mut renderer borrow
        // since `build_status_bar` reads ws.mux / self.cfg
        // immutably.
        let status = self.build_status_bar(ws);
        let layout = self.pane_layout(ws, area);
        let focus = ws.mux.active_focus();
        let window = ws.window.clone();

        // A failed acquire/render owns the next GPU attempt until its one-shot
        // deadline is polled in about_to_wait. This gate also stops unrelated
        // cursor/input/output redraw requests from bypassing the backoff. Do
        // the non-GPU state maintenance above, but neither snapshot terminal
        // contents nor touch an invisible surface except for one explicit
        // offscreen capture. That exception may bypass a transient retry, never
        // a pending renderer rebuild.
        let screenshot_may_render_offscreen = ws.renderer.as_ref().is_some_and(|renderer| {
            ws.window.as_ref().is_some_and(|window| {
                pending_screenshot_may_render_offscreen(
                    renderer.has_pending_screenshot(),
                    window.is_minimized(),
                    ws.window_shown,
                    window.is_visible(),
                )
            })
        });
        let recovery_blocks_render = ws.frame_recovery.renderer_rebuild_pending()
            || (ws.frame_recovery.has_pending() && !screenshot_may_render_offscreen);
        if !ws.dpi_resize.render_allowed()
            || recovery_blocks_render
            || (window_is_render_hidden(ws) && !screenshot_may_render_offscreen)
        {
            return;
        }

        if ws.renderer.is_none() {
            return;
        }

        // Re-open each pane's output latch only when this window is about to
        // sample generations for a real frame. Keeping the latch closed across
        // the entire deferred frame budget collapses a fast reader's many
        // chunks into one event-loop wake without losing output that races the
        // snapshot below. This must follow every early-return guard: entering
        // Presenting without attempting a frame would strand the pacer and
        // schedule a near-zero recovery loop.
        self.acknowledge_output_wakes(ws);
        // Is this paint flushing coalesced PTY output? Retained until the
        // frame outcome so the flood detector advances only after
        // presentation (see `effective_output_budget`).
        let was_coalescing_paint = ws.output_pacer.begin_frame();

        // Capture the output generations represented by this candidate frame
        // before taking any new pane snapshots. A hover-only frame reuses the
        // exact cached generations rather than sampling live generations after
        // the reuse check; output racing either path therefore causes one
        // harmless extra repaint, never a missed repaint. The recycled map is
        // committed only after the surface is presented.
        stage_output_generations_for_frame(
            &mut ws.pending_output_gen,
            &ws.seen_output_gen,
            reuse_pane_snapshots.then_some(&ws.pane_snapshot_keys),
            ws.mux
                .panes
                .iter()
                .map(|(id, pane)| (*id, pane.term.output_generation())),
        );

        // Capture each visible pane's renderable state into a pooled snapshot
        // under the Term lock, then drop the guard at once (a microsecond-scale
        // flat copy per pane). The renderer works from the snapshots, so the
        // GPU frame (shaping, surface acquire, submit, present; milliseconds)
        // never holds a Term mutex. Holding them across the frame starves the
        // PTY parser on `term.lock()` under output flood. Also pass the pane's
        // title so the per-pane titlebar can render it.
        let mut snaps = std::mem::take(&mut ws.pane_snapshots);
        let mut snapshot_keys = std::mem::take(&mut ws.pane_snapshot_keys);
        if !reuse_pane_snapshots {
            snapshot_keys.clear();
        }
        let mut metas = Vec::with_capacity(layout.len());
        let home = crate::mux::home_dir_string();
        for (id, r) in &layout {
            if let Some(p) = ws.mux.panes.get(id) {
                // Each protocol registry has its own defensive cap. Interleave
                // all three before renderer-side visibility culling so a busy
                // class cannot hide the others, while offscreen entries do not
                // consume the smaller frame-wide draw quota.
                let per_class_limit = kettle_core::GraphicsLimits::default().placements;
                let imgs = round_robin_bounded(
                    [
                        p.term.placements(),
                        p.term.placeholder_tiles(),
                        p.term.relative_tiles(),
                    ],
                    per_class_limit.saturating_mul(3),
                );
                let si = metas.len();
                let snapshot_ready = if reuse_pane_snapshots {
                    snaps.get(si).is_some()
                } else {
                    let output_generation = p.term.output_generation();
                    let Ok(g) = p.term.term.lock() else {
                        continue;
                    };
                    if snaps.len() <= si {
                        snaps.push(PaneSnapshot::default());
                    }
                    snaps[si].capture_with_card_marks(&g, p.inline_cards.needs_marks());
                    drop(g); // lock released — the render below is lock-free
                    snapshot_keys.push((
                        *id,
                        output_generation,
                        snaps[si].columns,
                        snaps[si].screen_lines,
                    ));
                    true
                };
                if snapshot_ready {
                    let cwd = p.term.current_dir_or_native();
                    let mut title_parts = pane_title_parts(
                        &self.cfg.agent_badge,
                        p.agent_attached,
                        p.read_only,
                        &p.title,
                        p.title_is_placeholder,
                        cwd.as_deref(),
                        home.as_deref(),
                    );
                    title_parts
                        .prefix
                        .push_str(&media_shelf_badge(p.media_shelf.unseen()));
                    metas.push((
                        *id,
                        *r,
                        Some(*id) == focus,
                        imgs,
                        title_parts.prefix,
                        title_parts.title,
                        title_parts.path,
                        snaps[si].columns as u16,
                        snaps[si].screen_lines as u16,
                        // Terminator parity (`icon_bell`): the pane's own bell
                        // latch drives its titlebar bell indicator.
                        p.bell,
                        ws.bell_flashes
                            .get(id)
                            .map_or(0.0, |rang| bell_flash_ramp(rang.elapsed())),
                        p.group_name.clone(),
                    ));
                }
            }
        }
        snaps.truncate(metas.len());
        snapshot_keys.truncate(metas.len());
        // Hand the renderer borrows into `metas` (which lives
        // for the whole frame) instead of a second per-pane clone of the images
        // Vec / title String / group_name. `snap` borrows the pooled snapshot
        // the same way; both outlive `panes`, which drops before the pool
        // returns to `ws.pane_snapshots`.
        // An on-to-off edge the scheduler marked hands the blink to the window
        // server (macOS) if this frame still draws the off phase with nothing
        // holding the cursor. Only a successful main present lets the renderer
        // draw the patch into the layer's surface afterward.
        let blink_handoff = ws.blink_layer.take_pending_handoff()
            && self.cfg.macos_cursor_blink_layer
            && !overlay.cursor_visible
            && overlay.window_focused
            && overlay.ime_preedit.is_none()
            && Self::pane_cursor_layer_eligible(ws)
            && Self::ensure_cursor_layer(ws);
        let panes: Vec<PaneView> = metas
            .iter()
            .zip(snaps.iter())
            .map(
                |(
                    (id, r, f, imgs, prefix, title, path, cols, rows, bell, bell_flash, group_name),
                    snap,
                )| {
                    PaneView {
                        id: *id,
                        rect: r.leaf,
                        terminal: r.terminal,
                        titlebar: r.titlebar,
                        snap,
                        focused: *f,
                        images: imgs.as_slice(),
                        inline_cards: ws.mux.panes.get(id).map(|pane| &pane.inline_cards),
                        tr: self.ui_text,
                        title_prefix: prefix.as_str(),
                        title: title.as_str(),
                        title_path: path.as_deref(),
                        size_cols: *cols,
                        size_rows: *rows,
                        bell: *bell,
                        bell_flash: *bell_flash,
                        group_name: group_name.as_deref(),
                    }
                },
            )
            .collect();
        let live_opacity_floor = ws
            .native_material
            .as_ref()
            .and_then(|material| material.live_opacity_floor());
        // Whether anything covers the cards, for the tip: no dialog, menu or
        // viewer over them.
        let cards_uncovered = !self.pointer_modal_open(ws);
        // Status bar and native fallback state are built BEFORE the &mut
        // renderer borrow (the helpers read other window state immutably).
        let Some(renderer) = ws.renderer.as_mut() else {
            drop(panes);
            ws.pane_snapshots = snaps;
            ws.pane_snapshot_keys = snapshot_keys;
            ws.output_pacer.presentation_failed();
            return;
        };
        // AppKit clips a decorated window's bottom corners after the Metal
        // scene is drawn. Tell the renderer to replace the square pane-border
        // strips at those two outer corners with one concentric antialiased
        // outline. Borderless/fullscreen windows have square content surfaces;
        // every non-macOS platform deliberately stays on the existing path.
        // Use live native state: AppKit's green button bypasses our actions,
        // and a config reload cannot retrofit an existing window's decorations.
        let live_fullscreen = window
            .as_ref()
            .is_some_and(|window| window.fullscreen().is_some());
        let live_decorated = window.as_ref().is_some_and(|window| window.is_decorated());
        renderer.set_rounded_window_corners(rounded_window_corner_policy(
            cfg!(target_os = "macos"),
            live_decorated,
            live_fullscreen,
        ));
        renderer.set_live_background_opacity_floor(live_opacity_floor);
        // Any frame while the window server blinks the cursor ends that blink.
        // The frame presents in one Core Animation transaction with hiding the
        // layer, so the screen never shows new content under a stale cursor,
        // or the old frame with no cursor.
        let layer_exit = ws.blink_layer.is_active();
        let exit_started = std::time::Instant::now();
        let mut layer_exit_presented = false;
        if layer_exit && let Some(layer) = &ws.cursor_layer {
            layer.begin_exit_frame();
        }
        if let Some(window) = window.as_ref() {
            let window = Arc::downgrade(window);
            renderer.ensure_background_redraw(move || {
                if let Some(window) = window.upgrade() {
                    window.request_redraw();
                }
            });
        }
        let frame_started = std::time::Instant::now();
        let frame_result = renderer.render_frame_with_status_and_pre_present(
            &panes,
            &tabbar,
            &self.cfg,
            &overlay,
            &status,
            || {
                if let Some(window) = &window {
                    window.pre_present_notify();
                }
            },
        );
        let frame_time = frame_started.elapsed();
        // A card has been seen once a frame showing it reached the screen.
        let presented = matches!(&frame_result, Ok(FrameOutcome::Presented));
        if presented
            && ws
                .card_sightings
                .note(renderer.painted_cards(), std::time::Instant::now())
        {
            // Assistive technology names each card on screen, and the
            // pointer may rest on one that moved or arrived.
            ws.accessibility_pending = true;
            ws.card_hover_stale = true;
        }
        if presented && overlay.update_toast.is_some() {
            self.update_toast_presented(ws.seq);
        }
        // The first card a user ever sees says a click opens it, once its
        // image is on screen with nothing over it: a card that loads in
        // place, or that a dialog or the viewer uncovers, qualifies then.
        let tip_begun = if presented && cards_uncovered {
            self.cards_tip.begin(
                renderer.shown_cards().map(|card| (card.pane, card.nonce)),
                std::time::Instant::now(),
            )
        } else {
            None
        };
        // C1 records only a presented scene. Reuse that scene immediately,
        // before any renderer setter can invalidate it, and with the same cfg.
        let cursor_patch = if blink_handoff && matches!(&frame_result, Ok(FrameOutcome::Presented))
        {
            Some(renderer.present_cursor_patch(&self.cfg))
        } else {
            None
        };
        // Return the snapshot pool (cell-Vec capacity recycles next frame).
        drop(panes);
        if let Some((pane, nonce)) = tip_begun
            && let Some(state) = ws.mux.panes.get_mut(&pane)
        {
            state.inline_cards.set_tip(Some(nonce));
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
        ws.pane_snapshots = snaps;
        ws.pane_snapshot_keys = snapshot_keys;
        match frame_result {
            Ok(outcome) => {
                let presented = apply_output_generation_outcome(
                    outcome,
                    &mut ws.seen_output_gen,
                    &mut ws.pending_output_gen,
                );
                match outcome {
                    FrameOutcome::Presented => {
                        debug_assert!(presented);
                        // Commit the rest of the frame transaction only after
                        // presentation. Updating the output generation before
                        // presenting would lose terminal damage whenever
                        // surface acquisition times out.
                        ws.frame_recovery.presented();

                        // Fallback reveal: normal startup reveals as soon as
                        // renderer init succeeds, then paints immediately.
                        // Keep this guard so any future path that reaches a
                        // rendered frame while still hidden cannot leave a
                        // visible-state window invisible forever. No-op after
                        // the first reveal and for `window_state = hidden`.
                        if !ws.window_shown {
                            if let Some(w) = &ws.window {
                                w.set_visible(true);
                            }
                            ws.window_shown = true;
                            if ws.startup_window {
                                crate::startup_trace::mark(
                                    crate::startup_trace::Phase::WindowRevealed,
                                );
                            }
                            log::info!(
                                "startup: window revealed at first paint, working set {:.1} MiB",
                                process_working_set_mb()
                            );
                        }
                        // The first frame is the first one the startup window
                        // presents: a frame that timed out or found the window
                        // occluded is not.
                        if ws.startup_window {
                            crate::startup_trace::mark(crate::startup_trace::Phase::FirstFrame);
                            crate::startup_trace::flush();
                        }
                        ws.last_paint = Some(std::time::Instant::now());
                        ws.output_pacer.presented();
                        // Track sustained-flood depth. A frame that flushed
                        // coalesced output bumps the counter; any other paint
                        // resets it. Failed presentation does neither because
                        // no output was actually flushed to the display.
                        ws.flood_paints = if was_coalescing_paint {
                            ws.flood_paints.saturating_add(1)
                        } else {
                            0
                        };
                        ws.blink_layer.note_presented(std::time::Instant::now());
                        if layer_exit {
                            // Commits with this frame's present in
                            // `end_exit_frame` below.
                            if let Some(layer) = &ws.cursor_layer {
                                layer.hide();
                            }
                            layer_exit_presented = true;
                        } else if blink_handoff {
                            if let Some(patch) = cursor_patch {
                                self.start_layer_blink(ws, patch, window.as_deref());
                            } else {
                                ws.blink_layer.failed_handoff();
                            }
                        }
                    }
                    FrameOutcome::RetryLater => {
                        debug_assert!(!presented);
                        ws.output_pacer.presentation_failed();
                        // A backend acquire timeout may already have blocked
                        // for close to a second; Outdated has just
                        // reconfigured the existing surface. In either case,
                        // retain damage and let the one-shot backoff schedule
                        // the next acquire.
                        ws.frame_recovery
                            .schedule_transient_retry(std::time::Instant::now());
                    }
                    FrameOutcome::Occluded => {
                        debug_assert!(!presented);
                        ws.output_pacer.presentation_failed();
                        // The wgpu result can precede winit's OS occlusion
                        // event. Keep a slow self-healing deadline for a
                        // spurious/transient result, while the state machine
                        // becomes fully quiescent as soon as the window system
                        // confirms hidden/minimized/occluded state.
                        ws.frame_recovery
                            .schedule_occluded_retry(std::time::Instant::now());
                    }
                    FrameOutcome::SurfaceLost => {
                        debug_assert!(!presented);
                        ws.output_pacer.presentation_failed();
                        // wgpu 30 requires Instance::create_surface here.
                        // Rebuild only this renderer on the healthy shared
                        // device; genuine device loss is handled separately by
                        // the process-wide recovery guard.
                        ws.frame_recovery
                            .schedule_renderer_rebuild(std::time::Instant::now());
                    }
                }
            }
            Err(e) => {
                ws.output_pacer.presentation_failed();
                log::warn!("render error: {e}");
                // Fallible glyph preparation can advance retained renderer
                // damage keys before returning (for example AtlasFull). A
                // plain redraw could therefore reuse inconsistent caches and
                // strand terminal damage. Rebuild the affected renderer on a
                // bounded schedule unless the device callback already routed
                // this failure into process-wide recovery.
                if !self.gpu.as_ref().is_some_and(|gpu| gpu.is_lost()) {
                    ws.frame_recovery
                        .schedule_renderer_rebuild(std::time::Instant::now());
                }
            }
        }
        // A frame that did not present leaves the layer blinking over the
        // off-phase frame beneath it, which is still what the screen shows.
        if layer_exit && let Some(layer) = &ws.cursor_layer {
            layer.end_exit_frame();
        }
        let cursor_exit_end = if cursor_exit_frame.is_some() && layer_exit_presented {
            crate::cursor_exit_log::raw_now_ns()
        } else {
            None
        };
        if layer_exit_presented {
            let exit_time = exit_started.elapsed();
            ws.blink_layer.exited(exit_time);
            if ws.cursor_exit_log.enabled {
                if let Some(line) = ws.cursor_exit_log.finish_frame(
                    cursor_exit_frame,
                    layer_exit_presented,
                    cursor_exit_end,
                ) {
                    crate::cursor_exit_log::emit_line(&line);
                }
            } else {
                log::info!(target: CURSOR_BLINK_LOG, "exit_frame_us={} render_frame_us={}",
                    exit_time.as_micros(), frame_time.as_micros());
            }
            if !self.cfg.macos_cursor_blink_layer {
                Self::retire_cursor_layer(ws);
            }
        }
    }

    /// Snapshot the focused `(tab, leaf)` pair. Paired with
    /// `note_focus_change` to detect whether an operation moved focus
    /// and, if so, reset the cursor blink phase so the new pane's
    /// cursor is visible immediately. Keyboard and mouse paths share it.
    fn focus_key(&self, ws: &WindowState) -> (usize, Option<u64>) {
        (ws.mux.active, ws.mux.active_focus())
    }

    /// If the focused `(tab, leaf)` changed since `pre`, land the
    /// cursor visible on the new pane right away.
    fn note_focus_change(&mut self, ws: &mut WindowState, pre: (usize, Option<u64>)) {
        if self.focus_key(ws) != pre {
            end_modal_composition(ws);
            // Focus moved to a pane, so not to a card any more.
            if ws.card_focus.take().is_some() {
                ws.accessibility_pending = true;
            }
            // Vi mode belongs to the pane that owns alacritty_terminal's
            // engine cursor. A focus change must not leave that old pane in
            // TermMode::VI while the UI routes keys to a different pane.
            self.exit_vi_mode(ws);
            self.reset_blink_phase(ws);
            // An open search bar follows focus (see `retarget_search_to_focus`).
            self.retarget_search_to_focus(ws);
            // Repaint immediately so the focused-pane border and the cursor's
            // solid/hollow state track the new pane. Otherwise a
            // focus-follows-mouse (`focus = sloppy`) change leaves the pane
            // under the cursor looking unfocused until some other event
            // triggers a redraw.
            if let Some(w) = &ws.window {
                w.request_redraw();
            }
        }
    }

    fn exit_vi_mode(&mut self, ws: &mut WindowState) {
        let Some(state) = ws.vi_mode.take() else {
            return;
        };
        if let Some(pane) = ws.mux.panes.get(&state.pane_id) {
            let mut term = pane
                .term
                .term
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            term.selection = None;
            if term.mode().contains(kettle_core::TermMode::VI) {
                term.toggle_vi_mode();
            }
        }
    }

    fn enter_vi_mode(&mut self, ws: &mut WindowState) {
        self.close_all_modals(ws);
        let Some(pane_id) = ws.mux.active_focus() else {
            return;
        };
        let Some(pane) = ws.mux.panes.get(&pane_id) else {
            return;
        };
        let mut term = pane
            .term
            .term
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        term.selection = None;
        if !term.mode().contains(kettle_core::TermMode::VI) {
            term.toggle_vi_mode();
        }
        drop(term);
        ws.vi_mode = Some(ViState {
            pane_id,
            visual: false,
        });
    }

    /// Close every modal overlay (search bar, command palette, settings,
    /// layout picker, hint mode, SSH launcher, context menu, title edit,
    /// vi mode, confirm dialog). Modal-opening actions call it first to avoid
    /// stacking two visible modals. A palette opened over the SSH launcher
    /// would render both, with only the palette capturing keys.
    fn close_all_modals(&mut self, ws: &mut WindowState) {
        if let Some(open) = ws.preview_overlay {
            self.close_preview(ws, open.pane);
        }
        // Opening or switching a modal takes pointer ownership. End a terminal
        // selection drag here so later motion cannot keep extending or
        // autoscrolling behind Command Palette, Settings, SSH, or title edit.
        self.finish_selection_gesture(ws);
        // A card held under a modal that opened is not clicked, even if the
        // modal is gone by the release.
        ws.card_press = None;
        // The modal that opens takes the keyboard from a preview lane, even
        // if it closes again before the event loop looks.
        self.exit_preview_focus(ws);
        // A live title edit materialises a chrome strip (see `tab_bar_h`), so
        // dropping it here changes the content rectangle. Resize only when it
        // was actually open -- this runs before every other modal opens, and an
        // unconditional resize would fire on all 17 call sites for nothing.
        let title_edit_was_open = ws.editing_title.is_some();
        self.close_search(ws);
        end_modal_composition(ws);
        ws.palette_input = None;
        ws.settings_nav = None;
        // Also drop any open inline settings text prompt (the image-path
        // editor) so it can't linger after the panel closes / reopens.
        ws.settings_text_edit = None;
        ws.layout_picker_input = None;
        ws.theme_picker_input = None;
        ws.hint_state = None;
        ws.ssh_input = None;
        ws.context_menu = None;
        ws.editing_title = None;
        // Vi mode is backed by per-terminal engine state, so closing the modal
        // must clear both the UI owner and TermMode::VI.
        self.exit_vi_mode(ws);
        // The confirm dialog ("Close this pane?", "Quit?") is a modal too.
        // Leaving it up would render two overlays with ambiguous key focus.
        // Clearing it here is safe because no confirm opener calls
        // close_all_modals after installing its dialog.
        self.cancel_confirm_dialog(ws);
        if title_edit_was_open {
            ws.pending_resize = true;
        }
    }

    /// Apply the in-progress title edit + clear the
    /// overlay. The scope decides which setter is invoked.
    fn apply_title_edit(&mut self, ws: &mut WindowState) {
        if let Some(state) = ws.editing_title.take() {
            ws.pending_resize = true;
            match state.scope {
                TitleEditScope::Window => {
                    let value = state.input;
                    // Empty clears the override and restores the
                    // `window-title-format` behaviour; anything else pins it
                    // as the base for `sync_window_title`.
                    ws.window_title_override = if value.trim().is_empty() {
                        None
                    } else {
                        Some(value)
                    };
                    self.sync_window_title(ws);
                }
                _ => {
                    apply_mux_title_edit(&mut ws.mux, &state);
                }
            }
        }
        self.hoover_groups(ws);
    }

    /// `true` while a modal owns the whole pointer surface. This is
    /// `any_modal_open` minus the search bar: the bar reserves its own lane
    /// below the grid and leaves the grid mouse-interactive, so it must not
    /// swallow pointer events the way the palette, settings, hints, SSH
    /// launcher, title editor, vi mode, or a confirm dialog do. Keyboard
    /// routing, focus-follows-mouse, and file drops keep using
    /// `any_modal_open`, because search does own the keyboard.
    fn pointer_modal_open(&self, ws: &WindowState) -> bool {
        self.non_search_modal_open(ws) || ws.preview_overlay.is_some()
    }

    /// Every modal that owns the keyboard and more: the search bar as well
    /// as those that own the pointer too. A preview lane holding the
    /// keyboard is not one: the pointer, the wheel and file drops still work
    /// everywhere else.
    fn any_modal_open(&self, ws: &WindowState) -> bool {
        ws.search.open || self.pointer_modal_open(ws)
    }

    /// Every modal except the search bar. The one list both
    /// `any_modal_open` and `pointer_modal_open` derive from, so the two
    /// cannot drift apart on anything but search.
    fn non_search_modal_open(&self, ws: &WindowState) -> bool {
        ws.palette_input.is_some()
            || ws.settings_nav.is_some()
            || ws.layout_picker_input.is_some()
            || ws.theme_picker_input.is_some()
            || ws.hint_state.is_some()
            || ws.ssh_input.is_some()
            || ws.context_menu.is_some()
            || ws.editing_title.is_some()
            || ws.vi_mode.is_some()
            // The confirm dialog is a modal too. Its keys have a dedicated
            // priority branch, but mouse, scroll, and cursor gating read this
            // list. Without it, clicks fall through to the terminal behind a
            // "Quit?" / "Close pane?" prompt.
            || ws.confirm_dialog.is_some()
    }

    fn overlay_text_line_height(&self, ws: &WindowState) -> f32 {
        ws.renderer
            .as_ref()
            .map(|renderer| renderer.overlay_text_line_height())
            .unwrap_or(self.menu_cell(ws).1)
    }

    fn overlay_text_cell_width(&self, ws: &WindowState) -> f32 {
        ws.renderer
            .as_ref()
            .map(|renderer| renderer.overlay_text_cell_width())
            .unwrap_or(self.menu_cell(ws).0)
    }

    /// Open a new tab running `argv`, inheriting the focused tab's
    /// current working directory. Shared by the new-tab `▾` dropdown's mouse +
    /// keyboard dispatch. Mirrors the `Action::NewTab` handler's pattern: log
    /// on failure, fire the `TabAdd` plugin event only when a tab was
    /// actually created.
    fn open_tab_with_argv(&mut self, ws: &mut WindowState, argv: &[String]) {
        let area = self.area(ws);
        let (cols, rows) = self.grid_of(ws, area);
        let geometry = self.pty_geometry_for_grid(ws, cols, rows);
        let waker = self.waker();
        let cwd = ws
            .mux
            .focused()
            .and_then(|p| p.term.current_dir_or_native());
        // Route through new_tab_with_launch so a WSL ▾-dropdown
        // entry's Linux cwd is carried via `wsl --cd` instead of being dropped
        // (a Windows spawn can't `cd` into a Linux path, so it fell back home).
        match ws
            .mux
            .new_tab_with_launch_geometry(&self.cfg, geometry, waker, argv.to_vec(), cwd)
        {
            Ok(()) => self.fire_tab_add_event(ws),
            Err(e) => log::warn!("could not open shell tab ({argv:?}): {e}"),
        }
    }

    /// Force the next redraw to render the cursor visible. Shared by:
    /// - the focus-change path (`note_focus_change`)
    /// - `Action::Reset` so a "fresh start" cursor is visible
    /// - `CursorBlinkingChange` events (DEC ?12 program-driven toggle)
    /// - the four modal Escape handlers so closing the
    ///   search/palette/hints/SSH overlay reveals the cursor immediately
    ///   instead of waiting up to one blink interval.
    fn reset_blink_phase(&mut self, ws: &mut WindowState) {
        let now = std::time::Instant::now();
        ws.blink_on = true;
        ws.last_blink = now;
        ws.last_blink_activity = now;
        // The window server's blink was planned from the old activity time.
        // A frame ends it (see `redraw`), so it never outlives new activity.
        if ws.blink_layer.is_active()
            && let Some(window) = &ws.window
        {
            window.request_redraw();
        }
    }

    /// Bring the app's blink phase to where the window server's animation is.
    /// A writer since the hand-off (`reset_blink_phase`, a DEC mode 12
    /// change) moves `last_blink` off the anchor, and its phase wins.
    fn materialize_layer_blink(ws: &mut WindowState, now: std::time::Instant) {
        let Some((on, edge)) = ws.blink_layer.materialize(ws.blink_on, ws.last_blink, now) else {
            return;
        };
        ws.blink_on = on;
        ws.last_blink = edge;
    }

    /// The window's cursor layer, installed and given to the renderer at the
    /// first hand-off, so startup never pays for it. A failure keeps the GPU
    /// blink for this window and logs once.
    fn ensure_cursor_layer(ws: &mut WindowState) -> bool {
        if ws.cursor_layer.is_some() {
            return true;
        }
        // A window between renderers (recovery) tries again at a later edge.
        let (Some(window), Some(renderer)) = (ws.window.as_deref(), ws.renderer.as_mut()) else {
            return false;
        };
        let installed = crate::macos_cursor_layer::CursorLayer::install(window).and_then(|layer| {
            layer.attach(renderer)?;
            Ok(layer)
        });
        match installed {
            Ok(layer) => {
                ws.cursor_layer = Some(layer);
                true
            }
            Err(reason) => {
                if ws.blink_layer.fall_back(reason) {
                    log::info!(
                        target: CURSOR_BLINK_LOG,
                        "window {}: the cursor blinks on the GPU: {reason}",
                        ws.seq
                    );
                }
                false
            }
        }
    }

    /// A hand-off frame reached the screen: start the window server's blink
    /// over the patch it drew, or keep the GPU blink for this edge.
    fn start_layer_blink(
        &self,
        ws: &mut WindowState,
        patch: CursorPatchOutcome,
        window: Option<&winit::window::Window>,
    ) {
        ws.blink_layer.failed_handoff();
        // A failed patch present may leave old contents on the layer.
        // Keep them hidden until both the patch and the animation are ready.
        if let Some(layer) = &ws.cursor_layer {
            layer.hide();
        }
        let rect = match patch {
            CursorPatchOutcome::Presented(rect) => rect,
            CursorPatchOutcome::Ineligible(reason) => {
                log::debug!(
                    target: CURSOR_BLINK_LOG,
                    "window {}: cursor patch ineligible: {reason:?}",
                    ws.seq
                );
                return;
            }
            CursorPatchOutcome::Failed(reason) => {
                log::debug!(
                    target: CURSOR_BLINK_LOG,
                    "window {}: cursor patch failed: {}",
                    ws.seq,
                    reason.as_str()
                );
                if reason == CursorPatchFailure::Lost {
                    Self::retire_cursor_layer(ws);
                }
                return;
            }
        };
        let rect_px = [rect.x, rect.y, rect.width, rect.height];
        let Some(plan) = crate::cursor_blink::layer_plan(
            ws.blink_on,
            ws.last_blink,
            ws.last_blink_activity,
            std::time::Duration::from_millis(self.cfg.cursor_blink_interval),
            self.cfg.cursor_blink_timeout(),
        ) else {
            return;
        };
        let (Some(layer), Some(window)) = (&ws.cursor_layer, window) else {
            return;
        };
        match layer.show(
            rect_px,
            window.scale_factor(),
            &plan,
            std::time::Instant::now(),
        ) {
            Ok(()) => ws.blink_layer.started(plan, rect_px),
            Err(reason) => log::debug!(
                target: CURSOR_BLINK_LOG,
                "window {}: the cursor layer did not start: {reason}",
                ws.seq
            ),
        }
    }

    /// Hide the cursor layer without a frame: the window lost focus, was
    /// occluded, changed size or scale, or is rebuilding its renderer. The Metal
    /// layer beneath shows the off phase, which is right for each of these
    /// until the next frame draws the phase materialized here.
    fn hide_cursor_layer(ws: &mut WindowState) {
        if !ws.blink_layer.is_active() {
            return;
        }
        Self::materialize_layer_blink(ws, std::time::Instant::now());
        if let Some(layer) = &ws.cursor_layer {
            layer.hide();
        }
        ws.blink_layer.hidden();
        if ws.cursor_exit_log.enabled {
            ws.cursor_exit_log.hidden();
        }
    }

    /// Remove the cursor layer and the renderer's surface on it. A rebuilt
    /// surface adds its Metal layer above the old one, so the next hand-off
    /// installs a fresh cursor layer above that.
    fn retire_cursor_layer(ws: &mut WindowState) {
        Self::hide_cursor_layer(ws);
        let layer = ws.cursor_layer.take();
        if layer.is_some()
            && let Some(renderer) = ws.renderer.as_mut()
        {
            renderer.detach_cursor_layer();
        }
    }

    /// Send a broadcast from `ws`, and then to the panes in every OTHER window
    /// that the same scope selects.
    ///
    /// A named broadcast group is a set the user declared, and `group_all`
    /// already spans every window the way Terminator's does. Stopping at the
    /// focused window would reach only part of a cross-window group while the
    /// other window's panes still show the group in their titlebars.
    ///
    /// Scopes that are defined by something window-local (`Tab`, and kettle's
    /// own window-wide `All`) do not walk, so this costs a single enum test
    /// when it does not apply.
    fn broadcast_input(
        &mut self,
        ws: &mut WindowState,
        bytes: &[u8],
        scroll_to_bottom: bool,
        input_time: std::time::Instant,
    ) -> PaneInputResult {
        let receipt_pane = Self::media_paste_receipt_pane(ws);
        let local = ws
            .mux
            .broadcast_write_delivery(bytes, scroll_to_bottom, receipt_pane);
        Self::dismiss_media_paste_receipt_after_delivery(ws, local);
        stamp_accepted_input(&mut ws.last_typed, local, input_time);
        let mut result = local.result;
        if crate::mux::Mux::scope_crosses_windows(&ws.mux.broadcast) {
            let scope = ws.mux.broadcast.clone();
            for other in self.windows.values_mut() {
                let receipt_pane = Self::media_paste_receipt_pane(other);
                let delivery = other.mux.broadcast_write_foreign_delivery(
                    &scope,
                    bytes,
                    scroll_to_bottom,
                    receipt_pane,
                );
                if scroll_to_bottom
                    && delivery.accepted
                    && let Some(window) = &other.window
                {
                    window.request_redraw();
                }
                Self::dismiss_media_paste_receipt_after_delivery(other, delivery);
                stamp_accepted_input(&mut other.last_typed, delivery, input_time);
                result = result.merge(delivery.result);
            }
        }
        result
    }

    fn populated_groups<'a>(&'a self, ws: Option<&'a WindowState>) -> HashSet<&'a str> {
        ws.into_iter()
            .chain(self.windows.values())
            .flat_map(|window| window.mux.panes.values())
            .filter_map(|pane| pane.group_name.as_deref())
            .collect()
    }

    fn hoover_groups(&mut self, ws: &mut WindowState) {
        if !any_group_scope_needs_hoover(std::iter::once(&*ws).chain(self.windows.values())) {
            return;
        }
        let populated = self.populated_groups(Some(ws));
        let reset_current =
            group_scope_is_globally_stale(ws.mux.autoclean_groups, &ws.mux.broadcast, &populated);
        let reset_windows: Vec<u64> = self
            .windows
            .iter()
            .filter_map(|(&seq, target)| {
                group_scope_is_globally_stale(
                    target.mux.autoclean_groups,
                    &target.mux.broadcast,
                    &populated,
                )
                .then_some(seq)
            })
            .collect();
        drop(populated);
        if reset_current {
            ws.mux.broadcast = BroadcastScope::Off;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
        for seq in reset_windows {
            if let Some(target) = self.windows.get_mut(&seq) {
                target.mux.broadcast = BroadcastScope::Off;
                if let Some(window) = &target.window {
                    window.request_redraw();
                }
            }
        }
    }

    fn hoover_mapped_groups(&mut self) {
        if !any_group_scope_needs_hoover(self.windows.values()) {
            return;
        }
        let populated = self.populated_groups(None);
        let reset_windows: Vec<u64> = self
            .windows
            .iter()
            .filter_map(|(&seq, target)| {
                group_scope_is_globally_stale(
                    target.mux.autoclean_groups,
                    &target.mux.broadcast,
                    &populated,
                )
                .then_some(seq)
            })
            .collect();
        drop(populated);
        for seq in reset_windows {
            if let Some(target) = self.windows.get_mut(&seq) {
                target.mux.broadcast = BroadcastScope::Off;
                if let Some(window) = &target.window {
                    window.request_redraw();
                }
            }
        }
    }

    fn handle_action(
        &mut self,
        ws: &mut WindowState,
        action: Action,
        event_loop: &ActiveEventLoop,
    ) {
        self.sync_pane_focus_reports(ws);
        let area = self.area(ws);
        let (cols, rows) = self.grid_of(ws, area);
        let tab_geometry = self.pty_geometry_for_grid(ws, cols, rows);
        let horizontal_split_geometry = self.prospective_split_geometry(ws, Dir::Horizontal, false);
        let vertical_split_geometry = self.prospective_split_geometry(ws, Dir::Vertical, false);
        let waker = self.waker();
        // Snapshot the (tab, pane-leaf) the cursor lives in so we can detect
        // any focus change the action causes.
        let pre_focus = self.focus_key(ws);
        let save_session = !matches!(
            action,
            Action::ScrollPageUp
                | Action::ScrollPageDown
                | Action::ScrollLineUp
                | Action::ScrollLineDown
                | Action::ScrollToTop
                | Action::ScrollToBottom
        );
        match action {
            Action::NewTab => {
                // Fire LuaEvent::TabAdd with the new active tab index (via
                // fire_tab_add_event) only when a tab was actually created, so
                // Lua never hears of a tab that doesn't exist. A PTY-spawn
                // failure is logged.
                match ws.mux.new_tab_geometry(&self.cfg, tab_geometry, waker) {
                    Ok(()) => self.fire_tab_add_event(ws),
                    Err(e) => log::warn!("could not open a new tab (shell spawn failed?): {e}"),
                }
            }
            Action::NewWindow => {
                // Open a real second window in this process, so tabs can move
                // live between windows and all windows share the GPU device. If
                // the window can't be created, fall back to a new tab (degraded
                // but useful).
                if let Err(_unopened) =
                    self.open_window(event_loop, WindowOpen::Fresh { cwd: None }, None, None)
                {
                    match ws.mux.new_tab_geometry(&self.cfg, tab_geometry, waker) {
                        Ok(()) => self.fire_tab_add_event(ws),
                        Err(e) => {
                            log::warn!("new-window fallback: could not open a tab: {e}")
                        }
                    }
                }
            }
            Action::SplitRight | Action::SplitLeft => {
                // Split Left puts the new pane first, as Ghostty's
                // `new_split:left` does.
                let new_first = action == Action::SplitLeft;
                let geometry = if new_first {
                    self.prospective_split_geometry(ws, Dir::Horizontal, true)
                } else {
                    horizontal_split_geometry
                };
                // If the focused pane has entered a shell it launched
                // (e.g. typed `wsl` in pwsh), clone THAT shell + its dir; else
                // clone the pane's own launch command.
                let detected = self.focused_foreground_shell(ws);
                let res = match detected {
                    Some(s) => ws.mux.split_with_geometry(
                        Dir::Horizontal,
                        new_first,
                        &self.cfg,
                        geometry,
                        waker,
                        s.argv,
                        s.cwd,
                    ),
                    None => ws.mux.split_geometry(
                        Dir::Horizontal,
                        new_first,
                        &self.cfg,
                        geometry,
                        waker,
                    ),
                };
                if let Err(e) = res {
                    report_split_failure(
                        if new_first { "left" } else { "right" },
                        &e,
                        &self.ui_text,
                    );
                } else {
                    self.restore_scaled_zoom_font(ws);
                }
            }
            Action::SplitDown | Action::SplitUp | Action::SplitAuto => {
                // `split_auto` cuts along the pane's LONGER axis, the way
                // Terminator does; `split_down` and `split_up` are always
                // vertical, and Split Up puts the new pane first.
                let dir = if action == Action::SplitAuto {
                    self.auto_split_dir(ws)
                } else {
                    Dir::Vertical
                };
                let new_first = action == Action::SplitUp;
                let geometry = if new_first {
                    self.prospective_split_geometry(ws, Dir::Vertical, true)
                } else if dir == Dir::Horizontal {
                    horizontal_split_geometry
                } else {
                    vertical_split_geometry
                };
                let detected = self.focused_foreground_shell(ws);
                let res = match detected {
                    Some(s) => ws.mux.split_with_geometry(
                        dir, new_first, &self.cfg, geometry, waker, s.argv, s.cwd,
                    ),
                    None => ws
                        .mux
                        .split_geometry(dir, new_first, &self.cfg, geometry, waker),
                };
                if let Err(e) = res {
                    report_split_failure(if new_first { "up" } else { "down" }, &e, &self.ui_text);
                } else {
                    self.restore_scaled_zoom_font(ws);
                }
            }
            Action::ClosePane => {
                // Prompt per pane when ask_before_closing = Always.
                // MultipleTerminals doesn't prompt (single pane); see
                // `should_prompt` for the matrix. Ghostty
                // `confirm-close-surface` parity: a pane idle at an
                // integrated-shell prompt has no work to lose, so skip the
                // confirm. Plain shells (no OSC 133 marks) never report idle.
                let busy = usize::from(
                    ws.mux
                        .focused()
                        .map(|p| !p.term.shell_idle())
                        .unwrap_or(true),
                );
                // Name the pane now — by the time the prompt is answered,
                // "focused" may mean a different one.
                if let Some(target) = ws.mux.active_focus()
                    && self.confirm_close(ws, 1, busy, ConfirmAction::ClosePane(target))
                {
                    return;
                }
                // Capture the focused pane id BEFORE the close —
                // afterward active_focus() returns the promoted sibling.
                let closing_pane = ws.mux.active_focus();
                if let Some(id) = closing_pane {
                    self.fire_pane_close_event(ws, id);
                }
                let was_last = ws.mux.close_focused();
                if was_last {
                    self.request_window_close(ws.seq);
                } else {
                    // Redraw and re-emit the focus event now. The split tree
                    // has collapsed (sibling promoted to root), but the
                    // renderer cache and the PaneFocus event's last-fired pane
                    // id stay stale until something schedules a frame. On
                    // Windows under wgpu DX12 this stale state was reported as
                    // a crash, most likely the tab-bar render path indexing a
                    // removed pane and panicking on its dropped
                    // Arc<Mutex<Terminal>> lock.
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    // Fire PaneFocus for the promoted sibling so plugins that
                    // observe focus drop stale per-pane state.
                    self.poll_focus_event(ws);
                }
            }
            Action::CloseTab => {
                // Close the
                // active tab via the modal when ask_before_closing
                // says so. scope_count = leaves in the active tab
                // (panes_in_tab below).
                let (panes_in_tab, busy_in_tab) = tab_close_scope(ws, ws.mux.active);
                if self.confirm_close(
                    ws,
                    panes_in_tab,
                    busy_in_tab,
                    ConfirmAction::CloseTabHolding(ws.mux.tab_anchor_panes(ws.mux.active)),
                ) {
                    return;
                }
                // Capture the active index BEFORE close
                // so the LuaEvent::TabClose payload is meaningful
                // (after close, ws.mux.active points at a
                // different tab), via the shared fire_tab_close_event
                // helper.
                let closing_idx = ws.mux.active;
                if ws.mux.close_tab() {
                    self.request_window_close(ws.seq);
                }
                self.fire_tab_close_event(ws, closing_idx);
            }
            Action::CloseWindow => {
                // Phase 5 of the confirm-dialog design:
                // intercept via the `should_prompt` helper.
                // When ask-before-closing fires, open the modal
                // with on_confirm=CloseWindow; the modal's Confirm
                // dispatch (in the key handler) re-runs the close
                // path below.
                let (scope, busy) = window_close_scope(ws);
                if self.confirm_close(
                    ws,
                    scope,
                    busy,
                    ConfirmAction::CloseWindow {
                        drop_panes: DropPanes::Yes,
                    },
                ) {
                    return;
                }
                // Unlike `CloseTab`, drop *every* tab and pane in this window,
                // not just the focused tab.
                self.close_window_now(ws, DropPanes::Yes);
            }
            Action::NextTab => ws.mux.next_tab(),
            Action::PrevTab => ws.mux.prev_tab(),
            Action::FocusNext => ws.mux.focus_cycle(area, true),
            Action::FocusPrev => ws.mux.focus_cycle(area, false),
            Action::FocusLeft => ws.mux.focus_dir(area, -1, 0),
            Action::FocusRight => ws.mux.focus_dir(area, 1, 0),
            // Terminator rearranges panes by dragging a terminal onto another.
            // This is the same rearrangement addressed by direction: take the
            // focused pane and put it beside whichever pane focus would have
            // moved to, on that side. The neighbour search is the one
            // `goto_split` uses, so the pane lands where you were looking.
            Action::MovePaneLeft
            | Action::MovePaneRight
            | Action::MovePaneUp
            | Action::MovePaneDown => {
                let (dx, dy, dir, before) = match action {
                    Action::MovePaneLeft => (-1, 0, Dir::Horizontal, true),
                    Action::MovePaneRight => (1, 0, Dir::Horizontal, false),
                    Action::MovePaneUp => (0, -1, Dir::Vertical, true),
                    _ => (0, 1, Dir::Vertical, false),
                };
                if let Some(target) = ws.mux.pane_in_direction(area, dx, dy)
                    && let Some(moving) = ws.mux.active_focus()
                    && ws.mux.move_pane_beside(moving, target, dir, before)
                {
                    // The rearrangement changes every pane's rect, so the PTYs
                    // have to hear about it, and the layout is worth keeping.
                    self.resize_all(ws);
                    self.save_session(ws);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                }
            }
            Action::FocusUp => ws.mux.focus_dir(area, 0, -1),
            Action::FocusDown => ws.mux.focus_dir(area, 0, 1),
            Action::ResizeLeft => ws.mux.resize_focus(Dir::Horizontal, -0.03),
            Action::ResizeRight => ws.mux.resize_focus(Dir::Horizontal, 0.03),
            Action::ResizeUp => ws.mux.resize_focus(Dir::Vertical, -0.03),
            Action::ResizeDown => ws.mux.resize_focus(Dir::Vertical, 0.03),
            Action::Copy => {
                // Terminator parity (terminatorlib/config.py
                // `smart_copy` + terminal.py:real_copy_clipboard):
                //   smart_copy = true  (default) → if no selection, skip
                //     the clipboard write so the existing clipboard
                //     content is preserved. Lets a user Ctrl+Shift+C
                //     without losing a previous copy when they forgot
                //     to highlight first.
                //   smart_copy = false → always trigger the copy. If
                //     there's no selection, write empty — this CLOBBERS
                //     the clipboard. Deliberate UX choice for users who
                //     want Ctrl+Shift+C to consistently mean "the
                //     clipboard now reflects the current selection
                //     (empty or not)" without smart heuristics.
                let selection_text = ws.mux.focused().and_then(|p| {
                    p.term
                        .term
                        .lock()
                        .ok()
                        .and_then(|t| t.selection_to_string())
                });
                let payload = match copy_clipboard_decision(selection_text, self.cfg.smart_copy) {
                    Ok(payload) => payload,
                    Err(error) => {
                        log::warn!("copy: could not prepare selection text: {error}");
                        return;
                    }
                };
                let mut copied = false;
                if let Some(s) = payload
                    && let Some(cb) = &mut self.clipboard
                {
                    let had_selection = !s.is_empty();
                    match cb.set_text(s) {
                        Ok(()) => copied = had_selection,
                        Err(error) => log::warn!("clipboard set_text failed (copy): {error}"),
                    }
                }
                // Terminator parity (terminatorlib/config.py:91
                // `clear_select_on_copy`): if the config asked, drop the
                // selection so the user sees the copy "took". Default
                // false matches Terminator's default — the selection
                // stays so re-Copy still works.
                if copied && self.cfg.clear_select_on_copy {
                    self.clear_selection_on_input(ws);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                }
            }
            Action::Paste => self.paste_clipboard(ws),
            // The font size is window-wide, so zooming all panes is the same
            // as zooming one.
            Action::IncreaseFontSize
            | Action::DecreaseFontSize
            | Action::ResetFontSize
            | Action::ZoomInAll
            | Action::ZoomOutAll
            | Action::ZoomNormalAll => {
                if let Some(r) = ws.renderer.as_mut() {
                    // Step the logical font size directly. Back-deriving it
                    // from `r.cell_h` (physical px) would double-apply the
                    // scale factor on HiDPI.
                    let new = match action {
                        Action::IncreaseFontSize | Action::ZoomInAll => r.font_size() + 1.0,
                        // `set_font_size` clamps to the renderer's 5..=72
                        // range; a floor of 6 here turned "decrease" at a
                        // configured 5 into an increase.
                        Action::DecreaseFontSize | Action::ZoomOutAll => r.font_size() - 1.0,
                        _ => self.cfg.font_size,
                    };
                    r.set_font_size(new);
                }
                // The user changed the font OUTSIDE a scaled-zoom
                // enter/exit, so the saved pre-zoom size is now stale — drop it
                // so a later zoom-out keeps this new chosen size instead of
                // reverting to the old one.
                ws.scaled_zoom_prev_font_size = None;
            }
            Action::StartSearch => {
                self.open_search(ws);
            }
            Action::ToggleBroadcastAll => {
                // Toggle: turn on the `broadcast-default` scope, or turn
                // broadcast off when that scope is already on. The explicit off
                // chord (`ToggleBroadcastOff`) still works.
                let on = broadcast_scope_for_default(self.cfg.broadcast_default);
                ws.mux.broadcast = if ws.mux.broadcast == on {
                    crate::mux::BroadcastScope::Off
                } else {
                    on
                };
            }
            Action::ToggleBroadcastOff => {
                ws.mux.broadcast = crate::mux::BroadcastScope::Off;
            }
            Action::ToggleBroadcastGroup => {
                // Toggle broadcast scope between Off and
                // Group(focused_pane.group_name). If focused
                // pane has no group, log + no-op.
                let focused_group = ws.mux.focused().and_then(|p| p.group_name.clone());
                let Some(group) = focused_group else {
                    log::info!(
                        "toggle-broadcast-group: focused pane has no group_name; \
                         use Action::CreateGroup or Action::GroupTab first"
                    );
                    return;
                };
                ws.mux.broadcast = match &ws.mux.broadcast {
                    crate::mux::BroadcastScope::Group(name) if name == &group => {
                        crate::mux::BroadcastScope::Off
                    }
                    _ => crate::mux::BroadcastScope::Group(group),
                };
            }
            Action::ToggleBroadcastWindow => {
                // Toggle window-wide broadcast on/off. Unlike
                // ToggleBroadcastAll, whose scope follows `broadcast-default`
                // (per-tab by default), this always targets every pane in every
                // tab.
                ws.mux.broadcast = match &ws.mux.broadcast {
                    crate::mux::BroadcastScope::All => crate::mux::BroadcastScope::Off,
                    _ => crate::mux::BroadcastScope::All,
                };
            }
            Action::ToggleZoom => {
                // A tab with one pane has nothing to expand: entering zoom
                // there would only leave a hidden flag that the first split
                // then silently cleared. A zoomed tab can always leave zoom.
                // ScaledZoom keeps its font step on a single pane.
                if ws.mux.is_zoomed() || ws.mux.active_pane_count() > 1 {
                    // Route through the helper ScaledZoom uses, so un-zooming
                    // via the plain toggle still restores or clears a pending
                    // scaled-zoom font baseline. See `toggle_zoom_with_scale`.
                    self.toggle_zoom_with_scale(ws, None);
                }
            }
            // Ghostty `equalize_splits` parity: rebalance the active tab's
            // split tree to equal pane areas, then push the new geometry into
            // the PTYs.
            Action::EqualizeSplits => {
                if let Some(t) = ws.mux.tabs.get_mut(ws.mux.active) {
                    t.root.equalize();
                }
                self.resize_all(ws);
                self.save_session(ws);
            }
            // Ghostty's `text:` action, and what gives `⌘⌫` something to mean.
            // Unlike the five other text-writing actions this goes through
            // `write_terminal_input` rather than `feed_input`: it stands in for
            // a keystroke, so it should clear the selection, scroll on
            // keystroke and reach every pane of a broadcast, exactly as typing
            // the same bytes would. That helper also invalidates the affected
            // pane's media-paste receipt, which every authored-input route owes.
            Action::SendText(text) => {
                let _ = self.write_terminal_input(ws, text.as_bytes());
            }
            Action::SendNewline => {
                // Terminator parity (`key_send_newline`): write a literal
                // `\n` for shell line-editors that consume Enter normally
                // but expect an explicit `\n` for line continuation
                // (multi-line readline prompts). Typed-input semantics, so
                // read-only drops it.
                self.write_focused_terminal_input(ws, b"\n");
            }
            // Terminator parity (`key_preferences` /
            // `key_preferences_keybindings`). Terminator opens a GUI
            // Preferences dialog; kettle's preferences live in its config file,
            // so the preferences keybind opens that file with the OS's
            // registered handler. If no config file is loaded (kettle started
            // with `--config` pointing at a missing path, or no profile
            // resolved), it falls back to `Config::default_path()`.
            Action::EditConfig => {
                let path = self
                    .config_path
                    .clone()
                    .or_else(kettle_config::Config::default_path);
                if let Some(path) = path {
                    let tr = self.ui_text;
                    system_opener::open(path.clone(), move |e| {
                        log::warn!("Action::EditConfig: failed to open {}: {e}", path.display());
                        fire_notify(
                            tr.text(kettle_i18n::Text::NotifyTitleConfigNotOpened),
                            tr.text(kettle_i18n::Text::NotifyBodyConfigOpenFailed),
                        );
                    });
                } else {
                    log::warn!(
                        "Action::EditConfig: no config path resolved \
                         (set $XDG_CONFIG_HOME or pass --config)"
                    );
                    fire_notify(
                        self.ui_text
                            .text(kettle_i18n::Text::NotifyTitleConfigNotOpened),
                        self.ui_text.text(kettle_i18n::Text::NotifyBodyConfigNoPath),
                    );
                }
            }
            // Preferences submenu toggles. Each dispatch updates `self.cfg` so
            // the change applies now, and writes `key = value` back to the
            // user's config file via the `persist_config_toggle` helper (atomic
            // temp+rename, comment-preserving, with a first-write backup at
            // `<config>.bak`). Re-opening the menu picks up the new state.
            Action::SetScrollbarAlways => {
                self.cfg.scrollbar = kettle_config::ScrollbarMode::Always;
                self.persist_pref("scrollbar", "always");
            }
            Action::SetScrollbarAuto => {
                self.cfg.scrollbar = kettle_config::ScrollbarMode::Auto;
                self.persist_pref("scrollbar", "auto");
            }
            Action::SetScrollbarNever => {
                self.cfg.scrollbar = kettle_config::ScrollbarMode::Never;
                self.persist_pref("scrollbar", "never");
            }
            // Close-confirmation policy. The confirm bar deliberately does NOT
            // carry a "don't ask again" answer: it decides the close in front
            // of you, while standing policy lives here, where the selected
            // radio IS the feedback and picking another row is the undo.
            Action::SetAskBeforeClosingAlways => {
                self.cfg.ask_before_closing = kettle_config::AskBeforeClosing::Always;
                self.persist_pref("ask-before-closing", "always");
            }
            Action::SetAskBeforeClosingMultiple => {
                self.cfg.ask_before_closing = kettle_config::AskBeforeClosing::MultipleTerminals;
                self.persist_pref("ask-before-closing", "multiple-terminals");
            }
            Action::SetAskBeforeClosingNever => {
                self.cfg.ask_before_closing = kettle_config::AskBeforeClosing::Never;
                self.persist_pref("ask-before-closing", "never");
            }
            Action::ToggleCursorBlink => {
                self.cfg.cursor_blink = !self.cfg.cursor_blink;
                // Turning blink on from the menu after the idle timeout
                // must show it blinking, not rest steady until a keystroke.
                self.reset_blink_phase(ws);
                self.persist_pref(
                    "cursor-blink",
                    if self.cfg.cursor_blink {
                        "true"
                    } else {
                        "false"
                    },
                );
            }
            Action::ToggleCopyOnSelect => {
                self.cfg.copy_on_select = !self.cfg.copy_on_select;
                self.persist_pref(
                    "copy-on-select",
                    if self.cfg.copy_on_select {
                        "true"
                    } else {
                        "false"
                    },
                );
            }
            Action::SetBellOff => {
                self.cfg.bell = kettle_config::BellMode::Off;
                self.persist_pref("bell", "off");
            }
            Action::SetBellVisual => {
                self.cfg.bell = kettle_config::BellMode::Visual;
                self.persist_pref("bell", "visual");
            }
            Action::SetBellAttention => {
                self.cfg.bell = kettle_config::BellMode::Attention;
                self.persist_pref("bell", "attention");
            }
            Action::SetBellBoth => {
                self.cfg.bell = kettle_config::BellMode::Both;
                self.persist_pref("bell", "both");
            }
            Action::ToggleMouseHide => {
                self.cfg.mouse_hide_while_typing = !self.cfg.mouse_hide_while_typing;
                self.persist_pref(
                    "mouse-hide-while-typing",
                    if self.cfg.mouse_hide_while_typing {
                        "true"
                    } else {
                        "false"
                    },
                );
            }
            // Open the README through the same background launcher as URL clicks.
            Action::ShowHelp => {
                let url = "https://github.com/Reddimus/kettle#readme";
                system_opener::open(url, move |e| {
                    log::warn!("Action::ShowHelp: failed to open {url}: {e}");
                });
            }
            // Terminator parity (`key_scaled_zoom`). Toggle pane zoom and scale
            // the font 1.5x so glyphs grow with the enlarged pane, then restore
            // the saved size on exit. `ToggleZoom` shares this save/restore via
            // `toggle_zoom_with_scale`, which reads the post-toggle zoom flag
            // and pairs save/restore through one `Option<f32>`, so mixing the
            // two actions keeps the state in step.
            Action::ScaledZoom => {
                self.toggle_zoom_with_scale(ws, Some(1.5));
            }
            Action::ToggleFullscreen => {
                if let Some(w) = &ws.window {
                    w.set_fullscreen(if w.fullscreen().is_some() {
                        None
                    } else {
                        Some(Fullscreen::Borderless(None))
                    });
                }
            }
            Action::ClearHistory => {
                // CSI 3 J (ED 3) clears scrollback only and keeps the visible
                // screen and grid state, unlike Reset (`\e c`, RIS), which
                // wipes everything. kitty / iTerm2 / WezTerm all expose this as
                // "Clear Scrollback" or similar. With group input on, clear
                // every pane the user is typing into, in every window the
                // broadcast scope reaches, not just the focused one.
                const CLEAR_SCROLLBACK: &[u8] = b"\x1b[3J";
                if ws.mux.is_broadcast_on() {
                    let targets = ws.mux.broadcast_target_ids();
                    apply_terminal_control(&ws.mux, &targets, CLEAR_SCROLLBACK);
                    if crate::mux::Mux::scope_crosses_windows(&ws.mux.broadcast) {
                        let scope = ws.mux.broadcast.clone();
                        for other in self.windows.values() {
                            let targets = other.mux.foreign_target_ids(&scope);
                            apply_terminal_control(&other.mux, &targets, CLEAR_SCROLLBACK);
                        }
                    }
                } else if let Some(pane_id) = ws.mux.active_focus() {
                    apply_terminal_control(&ws.mux, &[pane_id], CLEAR_SCROLLBACK);
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::Reset => {
                // RIS (`ESC c`) full-resets the engine: clears the grid,
                // restores DEC modes to defaults, drops the alt-screen back.
                // The PTY child then sees a fresh prompt next time it draws.
                // Kettle also owns UI state OUTSIDE the engine (selection,
                // scrollback display-offset, and the search, command palette,
                // hint mode and SSH launcher overlays), so sweep that too, or a
                // stale highlight or open modal survives the reset.
                if let Some(pane_id) = ws.mux.active_focus() {
                    // The reset turns mouse reporting off, so a held button's
                    // release goes to the program first.
                    self.release_held_buttons(ws, |held| held.pane == Some(pane_id));
                    apply_terminal_control(&ws.mux, &[pane_id], b"\x1bc");
                }
                self.clear_selection_on_input(ws);
                self.close_all_modals(ws);
                // Reset the blink phase so the cursor shows at once. Otherwise
                // a Reset during the blink-off phase hides the cursor for up to
                // one blink interval, and Reset is the chord users hit to
                // recover a visually-jammed terminal. Shares `reset_blink_phase`
                // with the focus-change and modal-close paths.
                self.reset_blink_phase(ws);
            }
            Action::ScrollPageUp
            | Action::ScrollPageDown
            | Action::ScrollLineUp
            | Action::ScrollLineDown
            | Action::ScrollToTop
            | Action::ScrollToBottom => {
                if let Some(p) = ws.mux.focused()
                    && let Ok(mut t) = p.term.term.lock()
                {
                    t.scroll_display(match action {
                        Action::ScrollPageUp => Scroll::PageUp,
                        Action::ScrollPageDown => Scroll::PageDown,
                        // `Scroll::Delta(+n)` scrolls *back* (toward older
                        // lines) — same sign convention the mouse-wheel
                        // path uses; line-up = +1, line-down = -1.
                        Action::ScrollLineUp => Scroll::Delta(1),
                        Action::ScrollLineDown => Scroll::Delta(-1),
                        Action::ScrollToTop => Scroll::Top,
                        _ => Scroll::Bottom,
                    });
                }
            }
            Action::SelectAll => {
                // Select the entire buffer (scrollback + screen). Mirrors
                // `apply_smart_selection`'s Selection-building, spanning the
                // grid-absolute buffer bounds.
                if let Some(p) = ws.mux.focused()
                    && let Ok(mut t) = p.term.term.lock()
                {
                    use kettle_core::Dimensions;
                    let (top, bottom) = selection_buffer_bounds(
                        t.grid().history_size(),
                        t.grid().screen_lines(),
                        t.grid().columns(),
                    );
                    let mut sel = kettle_core::Selection::new(
                        kettle_core::SelectionType::Simple,
                        top,
                        kettle_core::Side::Left,
                    );
                    sel.update(bottom, kettle_core::Side::Right);
                    t.selection = Some(sel);
                }
            }
            Action::SelectToTop | Action::SelectToBottom => {
                // Extend the selection to the first line / last cell of the
                // buffer (the AskUbuntu "select all in terminator" gesture,
                // Shift+Home / Shift+End), then scroll to reveal the new extent.
                let to_top = matches!(action, Action::SelectToTop);
                if let Some(p) = ws.mux.focused()
                    && let Ok(mut t) = p.term.term.lock()
                {
                    use kettle_core::Dimensions;
                    let (top, bottom) = selection_buffer_bounds(
                        t.grid().history_size(),
                        t.grid().screen_lines(),
                        t.grid().columns(),
                    );
                    let (edge, side) = if to_top {
                        (top, kettle_core::Side::Left)
                    } else {
                        (bottom, kettle_core::Side::Right)
                    };
                    match t.selection.as_mut() {
                        // Grow an existing selection's active end toward the
                        // edge, keeping its anchor (editor-style range growth).
                        Some(sel) => sel.update(edge, side),
                        // No selection yet: anchor at the cursor, extend to edge.
                        None => {
                            let anchor = t.grid().cursor.point;
                            let mut sel = kettle_core::Selection::new(
                                kettle_core::SelectionType::Simple,
                                anchor,
                                kettle_core::Side::Left,
                            );
                            sel.update(edge, side);
                            t.selection = Some(sel);
                        }
                    }
                    t.scroll_display(if to_top { Scroll::Top } else { Scroll::Bottom });
                }
            }
            Action::JumpPrevPrompt | Action::JumpNextPrompt => {
                let prev = matches!(action, Action::JumpPrevPrompt);
                if let Some(p) = ws.mux.focused() {
                    p.term.jump_to_prompt(prev);
                }
            }
            Action::OpenSsh => {
                self.close_all_modals(ws);
                ws.ssh_input = Some((String::new(), 0));
            }
            Action::CommandPalette => {
                self.close_all_modals(ws);
                ws.palette_input = Some((String::new(), 0));
            }
            // Open the in-app settings overlay (Ctrl+, / right-click
            // → Settings / palette "Open settings").
            Action::OpenSettings => {
                self.close_all_modals(ws);
                // Enumerate GPUs once for the Graphics device picker and cache
                // them for the session; a wgpu adapter walk is too heavy to
                // repeat per frame.
                if self.gpu_choices.is_empty() {
                    self.gpu_choices = kettle_render::detect_gpus()
                        .into_iter()
                        .map(|g| {
                            (
                                format!("{:x}:{:x}:{}", g.vendor, g.device, g.name),
                                format!("{} ({})", g.name, g.kind),
                            )
                        })
                        .collect();
                }
                ws.settings_restart_pending = false;
                ws.settings_nav = Some(crate::settings::SettingsNav::default());
            }
            // Terminator parity (layoutlauncher.py): open the runtime layout
            // picker. With no saved layouts the modal still opens, with a hint
            // to save one with `kettle --layout NAME`.
            Action::OpenLayoutPicker => {
                self.close_all_modals(ws);
                ws.layout_picker_entries = crate::session::Session::list_layouts();
                ws.layout_picker_input = Some((String::new(), 0));
            }
            Action::OpenThemePicker => self.open_theme_picker(ws),
            Action::OpenMediaShelf => self.open_media_shelf(ws),
            Action::PreviewClipboardPath => self.preview_copied_file(ws),
            Action::FocusPreview => self.focus_preview(ws),
            // The silent preview, labelled as such, never the video itself.
            Action::PreviewPlay => {
                if let Some(pane) = ws.mux.active_focus()
                    && ws.preview_panels.contains_key(&pane)
                {
                    self.toggle_lane_preview(ws, pane);
                }
            }
            Action::RenderClipboardAsDiagram => self.preview_copied_diagram(ws, false),
            Action::RenderSelectionAsDiagram => self.preview_copied_diagram(ws, true),
            Action::CopyAgentSetup => self.copy_agent_setup(),
            Action::PreviewNext
            | Action::PreviewPrevious
            | Action::PreviewNextDiagram
            | Action::PreviewPreviousDiagram
            | Action::ClosePreview
            | Action::PreviewSource
            | Action::PreviewCanvas
            | Action::PreviewCopy
            | Action::PreviewReload
            | Action::VideoPlay
            | Action::VideoMute
            | Action::PreviewZoomIn
            | Action::PreviewZoomOut
            | Action::PreviewFit => {
                if let Some(hit) = lane_action_hit(&action)
                    && let Some(pane) = ws.mux.active_focus()
                    && ws.preview_panels.contains_key(&pane)
                {
                    self.act_on_preview(ws, pane, hit);
                }
            }
            Action::HintMode | Action::PreviewLink => {
                let preview = matches!(action, Action::PreviewLink);
                let targets = self.collect_hints(ws, preview);
                if targets.is_empty() {
                    // Nothing in the pane to preview: say so, as the plain
                    // quick select's empty pane needs no word. A quick select
                    // already open stays as it was.
                    if preview {
                        let tr = self.ui_text;
                        fire_notify(
                            tr.text(kettle_i18n::Text::NotifyTitlePreviewNothing),
                            tr.text(kettle_i18n::Text::NotifyBodyPreviewNothing),
                        );
                    }
                } else {
                    self.close_all_modals(ws);
                    // The purpose goes with the targets it chose.
                    ws.hint_state = Some((targets, String::new()));
                    ws.hint_preview = preview;
                }
            }
            Action::ToggleViMode => {
                if ws.vi_mode.is_some() {
                    self.exit_vi_mode(ws);
                } else {
                    self.enter_vi_mode(ws);
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::OpenContextMenu => {
                if let Some(focus) = ws.preview_focus {
                    self.open_preview_menu(ws, focus.pane, None);
                } else {
                    let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
                    self.open_context_menu(ws, px, py, false);
                }
            }
            Action::UndoCloseTab => {
                let waker = self.waker();
                match ws
                    .mux
                    .undo_close_tab_geometry(&self.cfg, tab_geometry, waker)
                {
                    Ok(true) => {
                        self.resize_all(ws);
                        self.save_session(ws);
                    }
                    Ok(false) => {
                        log::debug!("undo_close_tab: ring is empty, nothing to restore");
                    }
                    Err(e) => log::error!("undo_close_tab failed: {e}"),
                }
            }
            Action::DuplicateTab => {
                let waker = self.waker();
                if let Err(e) =
                    ws.mux
                        .duplicate_focused_tab_geometry(&self.cfg, tab_geometry, waker)
                {
                    log::error!("duplicate_tab failed: {e}");
                } else {
                    self.resize_all(ws);
                    self.save_session(ws);
                }
            }
            Action::DuplicatePane => {
                let waker = self.waker();
                if let Err(e) = ws.mux.duplicate_focused_pane_geometry(
                    crate::mux::Dir::Horizontal,
                    &self.cfg,
                    horizontal_split_geometry,
                    waker,
                ) {
                    log::error!("duplicate_pane failed: {e}");
                } else {
                    self.restore_scaled_zoom_font(ws);
                    self.resize_all(ws);
                }
            }
            Action::NextTheme | Action::PrevTheme => {
                let fwd = matches!(action, Action::NextTheme);
                // Stay with the current theme's appearance, one similar
                // look at a time; switching light and dark is its own action.
                let name = kettle_config::Theme::cycle_by_look(&self.cfg.theme_name, fwd);
                self.set_runtime_theme_name(ws, name);
                if !self.persist_pref("theme", name) {
                    // Config-governed; notify on failure.
                    fire_notify(
                        self.ui_text
                            .text(kettle_i18n::Text::NotifyTitleThemeNotSaved),
                        self.ui_text.text(kettle_i18n::Text::NotifyBodyNotSaved),
                    );
                }
            }
            Action::ToggleLightDark => {
                if let Some(next) = pick_light_dark_target(
                    &self.cfg.theme_name,
                    &self.cfg.light_theme,
                    &self.cfg.dark_theme,
                ) {
                    self.set_runtime_theme_name(ws, &next);
                    if !self.persist_pref("theme", &next) {
                        // Config-governed; notify on failure.
                        fire_notify(
                            self.ui_text
                                .text(kettle_i18n::Text::NotifyTitleThemeNotSaved),
                            self.ui_text.text(kettle_i18n::Text::NotifyBodyNotSaved),
                        );
                    }
                } else {
                    log::warn!(
                        "toggle-light-dark: needs both light-theme and dark-theme in config (current: light={:?} dark={:?})",
                        self.cfg.light_theme,
                        self.cfg.dark_theme,
                    );
                }
            }
            Action::ToggleSessionLog => {
                // Terminator parity (`plugins/logger.py`):
                // toggle the focused pane's session log. This arm computes the
                // path; the persistence worker performs all filesystem I/O.
                // Routed through `Terminal::set_log_path` so secure file
                // preparation, the reader's lock-skip flag, and the bounded
                // writer stay in one transition without filesystem I/O here.
                if let Some(pane) = ws.mux.focused() {
                    if pane.term.log_enabled() {
                        match pane.term.set_log_path(None) {
                            Ok(_) => log::info!("toggle-session-log: stopped"),
                            Err(error) => {
                                log::warn!("toggle-session-log: stop failed: {error}");
                                fire_notify(
                                    self.ui_text
                                        .text(kettle_i18n::Text::NotifyTitleSessionLogError),
                                    self.ui_text
                                        .text(kettle_i18n::Text::NotifyBodySessionLogStopFailed),
                                );
                            }
                        }
                    } else {
                        let secs = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);
                        let cache = cache_dir_from_env(|k| std::env::var(k).ok());
                        let path = session_log_path(secs, std::process::id(), cache.as_deref());
                        // Propagate the config's strip-ANSI choice before the
                        // log goes live, so the first admitted read honors it.
                        if let Ok(mut strip) = pane.term.log_strip_ansi.lock() {
                            *strip = self.cfg.log_strip_ansi;
                        }
                        match pane.term.set_log_path(Some(path.clone())) {
                            Ok(_) => log::info!(
                                "toggle-session-log: queued asynchronous open for {}",
                                path.display()
                            ),
                            Err(error) => {
                                log::warn!(
                                    "toggle-session-log: start {} failed: {error}",
                                    path.display()
                                );
                                fire_notify(
                                    self.ui_text
                                        .text(kettle_i18n::Text::NotifyTitleSessionLogError),
                                    self.ui_text
                                        .text(kettle_i18n::Text::NotifyBodySessionLogStartFailed),
                                );
                            }
                        }
                    }
                }
            }
            Action::TakeScreenshot => {
                // Compute the output path, queue the
                // request, then fire a desktop notification so the
                // user knows where to look. Also compute
                // the focused pane's rect at dispatch time so the
                // screenshot crops to just that pane. Computed
                // BEFORE the `&mut ws.renderer` borrow to keep
                // the borrow window narrow.
                let area = self.area(ws);
                let active = ws.mux.active;
                let focus_id = ws.mux.tabs.get(active).map(|t| t.focus).unwrap_or(0);
                let crop = ws
                    .mux
                    .leaves(active, area)
                    .into_iter()
                    .find(|(id, _)| *id == focus_id)
                    .map(|(_, rect)| rect);
                if let Some(renderer) = ws.renderer.as_mut() {
                    let secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let cache = cache_dir_from_env(|k| std::env::var(k).ok());
                    let path = session_screenshot_path(secs, std::process::id(), cache.as_deref());
                    let path_str = path.display().to_string();
                    let proxy = self.proxy.clone();
                    let (completion, receiver) = std::sync::mpsc::channel();
                    let request = kettle_render::ScreenshotRequest {
                        out_path: path,
                        output_policy: kettle_render::ScreenshotOutputPolicy::PrivateState,
                        crop,
                        completion: Some(completion),
                        cancellation: None,
                        recovery_wake: Some(kettle_render::ScreenshotRecoveryWake::new(
                            move || {
                                let _ = proxy.send_event(UserEvent::Wakeup);
                            },
                        )),
                    };
                    if ws.screenshot_completion.is_none()
                        && renderer.set_pending_screenshot(request).is_ok()
                    {
                        ws.screenshot_completion = Some(receiver);
                        log::info!("take_screenshot: queued -> {path_str}");
                        fire_notify(
                            self.ui_text
                                .text(kettle_i18n::Text::NotifyTitleScreenshotQueued),
                            &path_str,
                        );
                    } else {
                        log::warn!("take_screenshot: another capture is in progress");
                        fire_notify(
                            self.ui_text
                                .text(kettle_i18n::Text::NotifyTitleScreenshotBusy),
                            self.ui_text
                                .text(kettle_i18n::Text::NotifyBodyScreenshotWait),
                        );
                    }
                }
            }
            Action::ReloadConfig => self.reload_config(ws),
            // Terminator's `move_tab` wraps at both ends of the bar; the drag
            // path clamps instead. See `Mux::nudge_active_tab`.
            Action::MoveTabLeft | Action::MoveTabRight => {
                let delta = if action == Action::MoveTabLeft { -1 } else { 1 };
                if ws.mux.nudge_active_tab(delta) {
                    self.save_session(ws);
                }
            }
            Action::NewTabShell(n) => {
                // Ctrl+Shift+N opens the Nth dropdown
                // entry (Windows Terminal's profile shortcuts). The list is
                // process-cached + prewarmed; out-of-range is a silent no-op,
                // same as GotoTab clamping.
                let shells = kettle_core::term::detect_shells();
                if let Some((_, argv)) = shells.get(n as usize) {
                    let argv = argv.clone();
                    self.open_tab_with_argv(ws, &argv);
                }
            }
            Action::About => self.open_about_panel(ws),
            Action::GotoTab(n) => {
                let i = n as usize;
                if i < ws.mux.tabs.len() {
                    ws.mux.active = i;
                    ws.mux.touch_active_tab_seen();
                }
            }
            // Persistent update actions for bindings and the palette.
            // Both are a debug-logged no-op while no notice is active, so a
            // bound key is harmless the rest of the time.
            Action::OpenUpdate => {
                if !self.open_update_release(ws) {
                    log::debug!("open_update: no update notice is active");
                }
            }
            Action::DismissUpdate => {
                if !self.dismiss_update_notice(ws) {
                    log::debug!("dismiss_update: no update notice is active");
                }
            }
            // Terminator parity: the Edit-title overlay. Each action opens it
            // pre-filled with the current title; Enter applies via the matching
            // setter, Esc cancels. It renders as a thin bar at the top of the
            // window, shaped like the `Action::CommandPalette` overlay.
            Action::EditWindowTitle => {
                self.close_all_modals(ws);
                ws.editing_title = begin_title_edit(
                    &ws.mux,
                    TitleEditScope::Window,
                    GroupBulkScope::Single,
                    &ws.last_title,
                );
                ws.pending_resize = true;
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::EditTabTitle => {
                self.close_all_modals(ws);
                ws.editing_title = begin_title_edit(
                    &ws.mux,
                    TitleEditScope::Tab,
                    GroupBulkScope::Single,
                    &ws.last_title,
                );
                ws.pending_resize = true;
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::EditPaneTitle => {
                self.close_all_modals(ws);
                ws.editing_title = begin_title_edit(
                    &ws.mux,
                    TitleEditScope::Pane,
                    GroupBulkScope::Single,
                    &ws.last_title,
                );
                ws.pending_resize = true;
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::EditPaneGroup | Action::CreateGroup => {
                // Edit the focused pane's
                // broadcast-group name. Empty input → clear the
                // group. Same overlay mechanism as
                // EditPaneTitle. `CreateGroup` (Terminator name)
                // and `EditPaneGroup` (kettle name) share dispatch.
                self.close_all_modals(ws);
                ws.editing_title = begin_title_edit(
                    &ws.mux,
                    TitleEditScope::Group,
                    GroupBulkScope::Single,
                    &ws.last_title,
                );
                ws.pending_resize = true;
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::GroupTab | Action::GroupWindow => {
                // Open the
                // title-edit overlay with `bulk` set to Tab/Window
                // so on Apply the typed name writes to every pane
                // in scope.
                self.close_all_modals(ws);
                let bulk = if matches!(action, Action::GroupTab) {
                    GroupBulkScope::Tab
                } else {
                    GroupBulkScope::Window
                };
                ws.editing_title =
                    begin_title_edit(&ws.mux, TitleEditScope::Group, bulk, &ws.last_title);
                ws.pending_resize = true;
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::GroupAll | Action::UngroupAll | Action::ToggleGroupAll => {
                // Terminator parity (window.py:933 / :947). `group_all` puts
                // every terminal into a group literally named "All"; there is
                // no prompt, which is what separates it from `GroupWindow`.
                // `ungroup_all` is its partner.
                let all = kettle_ui_group_all_name();
                // `group_all_toggle` asks the INVOKING terminal, not the whole
                // set (window.py:940-945: `if widget.group == 'All'`). Testing
                // "is everything already grouped" instead inverted the answer
                // whenever one sibling had been ungrouped by hand: Terminator
                // ungroups everything, kettle grouped the straggler.
                let group = match action {
                    Action::GroupAll => true,
                    Action::UngroupAll => false,
                    _ => ws.mux.focused().and_then(|p| p.group_name.as_deref()) != Some(all),
                }
                .then(|| all.to_string());
                // Terminator's scope is its whole terminal collection, across
                // every window (`self.terminator.terminals`), so a second
                // kettle window must not be left out.
                for target in std::iter::once(&mut *ws).chain(self.windows.values_mut()) {
                    let pane_ids: Vec<u64> = target.mux.panes.keys().copied().collect();
                    for id in pane_ids {
                        if let Some(p) = target.mux.panes.get_mut(&id) {
                            p.group_name = group.clone();
                        }
                    }
                    if let Some(w) = &target.window {
                        w.request_redraw();
                    }
                }
            }
            Action::ToggleGroupTab | Action::ToggleGroupWindow => {
                // Terminator parity (window.py:959, :987). It tests the
                // INVOKING terminal's group and generates the name rather than
                // asking for one — `Tab N` / `Window group N` — which is what
                // makes these a toggle at all, where kettle's `GroupTab` and
                // `GroupWindow` open the name prompt.
                let tab_scope = matches!(action, Action::ToggleGroupTab);
                let name = if tab_scope {
                    format!("Tab {}", ws.mux.active + 1)
                } else {
                    format!("Window group {}", ws.seq)
                };
                let grouped = ws
                    .mux
                    .focused()
                    .and_then(|p| p.group_name.as_deref())
                    .is_some_and(|g| g == name);
                let pane_ids: Vec<u64> = if tab_scope {
                    ws.mux
                        .tabs
                        .get(ws.mux.active)
                        .map(|t| t.root.leaf_ids())
                        .unwrap_or_default()
                } else {
                    ws.mux.panes.keys().copied().collect()
                };
                for id in pane_ids {
                    if let Some(p) = ws.mux.panes.get_mut(&id) {
                        p.group_name = (!grouped).then(|| name.clone());
                    }
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            Action::UngroupTab | Action::UngroupWindow => {
                // Bulk-
                // clear the group on every pane in scope. No
                // overlay needed — empty input is the "clear"
                // signal, and the action carries the scope.
                let pane_ids: Vec<u64> = if matches!(action, Action::UngroupTab) {
                    ws.mux
                        .tabs
                        .get(ws.mux.active)
                        .map(|t| t.root.leaf_ids())
                        .unwrap_or_default()
                } else {
                    ws.mux.panes.keys().copied().collect()
                };
                for id in pane_ids {
                    if let Some(p) = ws.mux.panes.get_mut(&id) {
                        p.group_name = None;
                    }
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            // Terminator parity (terminatorlib/terminal.py:
            // key_next_profile + key_previous_profile): runtime cycle
            // through profile files at <config-dir>/profiles/.
            //
            // Enumerates the profiles directory each dispatch (cheap;
            // typically <10 entries), sorts deterministically, finds
            // the current entry by basename match against the loaded
            // config-path, and loads the next/prev. Falls back to
            // log::info when no profiles directory or no entries.
            Action::NextProfile | Action::PrevProfile => {
                // Delegate listing + name extraction to
                // kettle-config so the same path math has a single
                // home (and drift guards on it). Empty list → no-op
                // with a one-line info nudge.
                let names = kettle_config::Config::list_profiles();
                if names.is_empty() {
                    log::info!(
                        "{action:?}: no profiles in <config-dir>/profiles/ — \
                         create one with `kettle --print-default-config > \
                         ~/.config/kettle/profiles/dev.config`"
                    );
                } else {
                    let current = self
                        .config_path
                        .as_deref()
                        .and_then(kettle_config::Config::profile_name_from_path);
                    let next = pick_next_profile(
                        current.as_deref(),
                        &names,
                        matches!(action, Action::NextProfile),
                    );
                    if let Some(p) = kettle_config::Config::path_for_profile(&next) {
                        self.switch_profile(ws, p);
                    }
                }
            }
            // Split-tree rotation (Terminator `rotate_cw` / `rotate_ccw`):
            // turn the active tab's whole layout a quarter turn. No-op on a
            // single-pane tab. Rotating changes every pane's rect, so the PTYs
            // have to be resized and the new arrangement saved — a redraw alone
            // would leave every child process believing its old geometry.
            Action::RotateCw | Action::RotateCcw => {
                if ws.mux.rotate_layout(action == Action::RotateCw) {
                    self.restore_scaled_zoom_font(ws);
                    self.resize_all(ws);
                    self.save_session(ws);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                }
            }
            // Runtime scrollbar toggle. Cycles
            // ScrollbarMode through Never → Always → Auto → Never.
            // Three-state cycle (vs binary) because Auto is the
            // useful steady state for most users; explicit toggle
            // is for power users with a specific preference. Same
            // shape as the `NextTheme` cycle.
            Action::ToggleScrollbar => {
                use kettle_config::ScrollbarMode::*;
                self.cfg.scrollbar = match self.cfg.scrollbar {
                    Never => Always,
                    Always => Auto,
                    Auto => Never,
                };
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            // Terminator parity: toggle the focused pane's read-only
            // state. While on, user input (keystrokes / paste / broadcast) is
            // dropped before it reaches the PTY; the child keeps producing
            // output. A `[RO]` titlebar badge shows the state.
            Action::TogglePaneReadOnly => {
                // A pane turned read-only gets no more input, so a held
                // button's release goes to it first.
                if let Some(pane) = ws.mux.active_focus()
                    && ws.mux.focused().is_some_and(|focused| !focused.read_only)
                {
                    self.release_held_buttons(ws, |held| held.pane == Some(pane));
                }
                let _ = ws.mux.toggle_focused_read_only();
                // The `[RO]` titlebar badge reflects the new state.
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            // Insert pane index. Pane index is 1-based
            // (matches Terminator's GotoTab + every user-facing
            // numbering). InsertPanePadded uses 2-digit zero-padded
            // form (Terminator default).
            Action::InsertPaneNumber => {
                let idx = ws
                    .mux
                    .focused_pane_index_in_tab()
                    .map(|i| i + 1)
                    .unwrap_or(1);
                self.write_focused_terminal_input(ws, idx.to_string().as_bytes());
            }
            Action::InsertPanePadded => {
                let idx = ws
                    .mux
                    .focused_pane_index_in_tab()
                    .map(|i| i + 1)
                    .unwrap_or(1);
                self.write_focused_terminal_input(ws, format!("{idx:02}").as_bytes());
            }
            // Terminator parity (`insert_term_name.py`
            // plugin → `InsertTermName` menu item + keybind). Send
            // the focused pane's title (Pane::title — same string
            // the chrome shows in the per-pane titlebar) to the
            // PTY. Useful for scripts that want to label their
            // output by source pane, or for keyboard workflows
            // that re-type the current title into the command line.
            Action::InsertPaneName => {
                if let Some(title) = ws.mux.focused().map(|pane| pane.title.clone()) {
                    self.write_focused_terminal_input(ws, title.as_bytes());
                }
            }
            // Terminator parity (`dir_open.py` plugin → `CurrDirOpen` menu
            // item). Open the focused pane's cwd in the OS file manager by
            // routing `file://<cwd>` through `open_url`, like a clicked
            // `file://` hyperlink, so the `LuaEvent::UrlClicked` dispatch, the
            // `custom_url_handler` config and the `is_safe_url` allowlist
            // (which accepts `file://` without `..`) all apply.
            Action::OpenCwdInFileManager => {
                match ws
                    .mux
                    .focused()
                    .and_then(|p| p.term.current_dir_or_native())
                {
                    // Refuse a non-local OSC 7 cwd before
                    // building/opening the URL (it's untrusted PTY input — a
                    // UNC path would trigger an SMB/NTLM leak on Windows). A
                    // remote pane's directory is on another machine.
                    // The URL is encoded from the path, so a directory with a
                    // space or `#` in its name opens rather than failing the
                    // URL check or naming another folder.
                    Some(cwd) if cwd_is_local(&cwd) => {
                        let pane = ws.mux.active_focus();
                        match kettle_core::links::file_url_for_path(std::path::Path::new(&cwd)) {
                            Some(url) => self.open_pane_link(ws, pane, &url),
                            None => log::warn!(
                                "Action::OpenCwdInFileManager: the cwd has no local file URL"
                            ),
                        }
                    }
                    Some(_) => {
                        log::warn!(
                            "Action::OpenCwdInFileManager: refusing a non-local cwd \
                             reported via OSC 7 (possible UNC/SSRF payload)"
                        );
                    }
                    None => {
                        log::info!("Action::OpenCwdInFileManager: focused pane has no known cwd");
                    }
                }
            }
            // Half-page scroll. Same shape as the
            // ScrollPageUp/Down handler but with half the row count.
            // Pull the row count from the focused pane's grid
            // dimensions (works for any pane size).
            Action::ScrollPageUpHalf | Action::ScrollPageDownHalf => {
                if let Some(p) = ws.mux.focused()
                    && let Ok(mut t) = p.term.term.lock()
                {
                    use kettle_core::Dimensions;
                    let rows = t.screen_lines() as i32;
                    let half = (rows / 2).max(1);
                    let dir = if matches!(action, Action::ScrollPageUpHalf) {
                        half
                    } else {
                        -half
                    };
                    t.scroll_display(Scroll::Delta(dir));
                }
            }
            // Paste the X11 PRIMARY selection (middle-click). On X11
            // PRIMARY is the last mouse-highlighted text, distinct from the
            // CLIPBOARD; `paste_primary` reads PRIMARY on Linux and falls back
            // to the clipboard on Wayland/macOS/Windows. It shares `paste_text`
            // so the LOCAL_PASTE_MAX clamp, bracketed-paste wrap, and broadcast
            // scoping all match Action::Paste.
            Action::PastePrimary => self.paste_primary(ws),
            // In-process Quake toggle. Same tri-state
            // logic as the `--toggle` remote command:
            //   hidden → show + focus
            //   visible + focused → hide
            //   visible + !focused → focus (don't hide)
            Action::ToggleWindowVisibility => {
                if let Some(w) = &ws.window {
                    let visible = w.is_visible().unwrap_or(true);
                    let focused = w.has_focus();
                    if !visible {
                        w.set_visible(true);
                        w.focus_window();
                    } else if focused {
                        w.set_visible(false);
                    } else {
                        w.focus_window();
                    }
                }
            }
            // Terminator parity (`detachable_tabs`): move the active tab into a
            // new window.
            Action::MoveTabToWindow => self.open_tab_window_chooser(ws),
            Action::MoveTabToNewWindow => {
                // Move the tab LIVE, in process. Its panes (PTYs, scrollback,
                // running programs) transfer untouched to a brand-new window
                // via detach_tab → open_window(AdoptTab). The deprecated
                // receive-side `--tab-handoff` parsing remains for an old sender
                // caught mid-upgrade.
                if !self.cfg.detachable_tabs {
                    log::info!("move_tab_to_new_window ignored because detachable-tabs = false");
                    return;
                }
                if ws.mux.tabs.len() <= 1 {
                    // Moving a lone tab out of its window is a no-op: you'd
                    // get the same window back.
                    return;
                }
                let closing_idx = ws.mux.active;
                self.release_held_buttons_in_tab(ws, closing_idx);
                let Some(dt) = ws.mux.detach_tab(closing_idx) else {
                    return;
                };
                self.release_focus_report_to(ws, &dt);
                let paste = ws.take_moved_paste_state(&dt.tab, std::time::Instant::now());
                match self
                    .open_window(
                        event_loop,
                        WindowOpen::AdoptTab {
                            tab: dt,
                            source_color: self.tear_off_source_color(ws),
                            font_size: ws.renderer.as_ref().map(|r| r.font_size()),
                            paste: Box::new(paste),
                        },
                        None,
                        None,
                    )
                    .map_err(|open| *open)
                {
                    Ok(_) => {
                        // The tab LEFT this window, so plugins see a tab-close
                        // event.
                        self.fire_tab_close_event(ws, closing_idx);
                        // Agents subscribed to the event feed see moves.
                        self.ctl_broadcast(
                            "tab_moved",
                            None,
                            serde_json::json!({
                                "from_window": ws.seq,
                                "to_window": self.focused_seq,
                                "tab": closing_idx,
                            }),
                        );
                        self.resize_all(ws);
                        if let Some(w) = &ws.window {
                            w.request_redraw();
                        }
                    }
                    Err(WindowOpen::AdoptTab { tab: dt, paste, .. }) => {
                        // Window creation failed — put the live tab back
                        // exactly where it was; nothing is lost.
                        log::warn!(
                            "MoveTabToNewWindow: open_window failed; tab kept in source window"
                        );
                        ws.mux.attach_tab(dt, Some(closing_idx));
                        ws.restore_moved_paste_state(*paste);
                    }
                    Err(_) => unreachable!("open_window returns the WindowOpen it was given"),
                }
            }
            Action::ResetAndClear => {
                // Terminator parity (key_reset_clear): Reset (RIS, \ec) and
                // ClearHistory (CSI 3 J) in one keybind, with Reset's sweep of
                // Kettle's own UI state.
                if let Some(pane_id) = ws.mux.active_focus() {
                    // As for Reset, a held button's release goes first.
                    self.release_held_buttons(ws, |held| held.pane == Some(pane_id));
                    // One request, so a full queue cannot accept RIS but reject
                    // the paired history clear.
                    apply_terminal_control(&ws.mux, &[pane_id], b"\x1bc\x1b[3J");
                }
                self.clear_selection_on_input(ws);
                self.close_all_modals(ws);
                self.reset_blink_phase(ws);
            }
        }
        // If focus moved as a result of the action,
        // land the cursor visible on the new pane right away.
        self.hoover_groups(ws);
        self.note_focus_change(ws, pre_focus);
        // Coalesce into the per-frame flush when a frame is actually coming.
        //
        // This tail runs after EVERY action. Resizing here eagerly meant a
        // queued pair of geometry-affecting actions -- a Lua hook dispatching
        // `EditWindowTitle` then `CommandPalette`, processed before any redraw
        // -- resized twice: shrink, then grow, for a net-zero frame. The user
        // sees nothing, but the PTY child gets two `SIGWINCH`s and vim, tmux or
        // htop redraw for no reason.
        //
        // `save_session` runs immediately below and does NOT depend on this:
        // it records the OS window rect and the split tree, never the PTY grid.
        //
        // With no window there is no frame to flush on, so resize now — a
        // deferred resize that never runs is worse than an eager one that runs
        // twice.
        if ws.window.is_some() {
            ws.pending_resize = true;
        } else {
            self.resize_all(ws);
        }
        if save_session {
            self.save_session(ws);
        }
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
    }

    /// Terminator plugin parity (`LuaEvent::PaneFocus`).
    /// Detect focus boundary crossings —
    /// from any source (keybind, mouse click, new tab, close tab,
    /// remote-control IPC) — and emit a single `PaneFocus` event
    /// per crossing.
    ///
    /// Polled from `redraw()` per tick rather than wiring every
    /// focus-changing call site because (a) there are 6+
    /// such sites today and (b) future changes will add more.
    /// One diff site = one drift guard, not N.
    ///
    /// First tick after startup emits with `previous = None` so
    /// plugins can seed their state.
    fn poll_focus_event(&mut self, ws: &mut WindowState) {
        let current = ws.mux.active_focus();
        let Some(cur_id) = current else { return };
        if ws.last_emitted_focus == Some(cur_id) {
            return;
        }
        let prev = ws.last_emitted_focus;
        ws.last_emitted_focus = Some(cur_id);
        self.fire_lua_event(
            ws,
            crate::LuaEvent::PaneFocus(prev, cur_id),
            "pane_focus hook",
        );
        // Agent-first A2: mirror the focus change to ctl subscribers.
        self.ctl_broadcast(
            "pane_focus",
            Some(cur_id),
            serde_json::json!({"previous": prev}),
        );
    }

    /// Reflect the FOCUSED pane's OSC 9;4 progress onto the OS
    /// taskbar button each frame (pwsh 7 / Windows Terminal parity). Reads the
    /// focused pane the same way the cursor-blink poll does; `Taskbar` dedups
    /// internally, so an unchanged value costs nothing. No-op off Windows.
    fn poll_taskbar_progress(&mut self, ws: &mut WindowState) {
        let progress = ws
            .mux
            .active_focus()
            .and_then(|id| ws.mux.panes.get(&id))
            .and_then(|p| p.term.progress());
        if let Some(window) = ws.window.clone() {
            ws.taskbar.apply(&window, progress);
        }
    }

    /// Terminator plugin parity (`LuaEvent::TitleChanged`). Walk live panes,
    /// diff each title against `ws.last_emitted_titles`, emit on any
    /// boundary cross. One pass site, regardless of how many
    /// title-mutating sites exist in App.
    ///
    /// O(n_panes) per redraw. Even 100 panes is trivial — a
    /// hash lookup + string compare per entry. A follow-up
    /// can add a "dirty-title" bitset on Mux if pane counts
    /// grow into the thousands.
    fn poll_title_event(&mut self, ws: &mut WindowState) {
        // Also run when a ctl subscriber is present, so title changes reach
        // agents even without a Lua engine.
        let has_subscribers = self
            .ctl
            .as_ref()
            .map(|c| c.has_subscribers())
            .unwrap_or(false);
        if self.lua_engine.is_none() && !has_subscribers {
            return;
        }
        let mut changes: Vec<(u64, String)> = Vec::new();
        for (id, p) in ws.mux.panes.iter() {
            let last = ws.last_emitted_titles.get(id);
            if last.map(|s| s.as_str()) != Some(p.title.as_str()) {
                changes.push((*id, p.title.clone()));
            }
        }
        for (id, title) in changes {
            ws.last_emitted_titles.insert(id, title.clone());
            self.fire_lua_event(
                ws,
                crate::LuaEvent::TitleChanged(id, title.clone()),
                "title_changed hook",
            );
            self.ctl_broadcast("title", Some(id), serde_json::json!({"title": title}));
        }
        // Drop title state for panes that have closed so this map
        // can't grow unbounded over a long session of opening/closing panes
        // (it's only ever read for live panes). Covers every close path —
        // keybind, confirm dialog, tab close, reap. The O(1) length guard means
        // the O(n) retain runs only when there are actually stale entries.
        if ws.last_emitted_titles.len() > ws.mux.panes.len() {
            let panes = &ws.mux.panes;
            ws.last_emitted_titles
                .retain(|id, _| panes.contains_key(id));
        }
    }

    fn request_all_window_redraws(&self, ws: &WindowState) {
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
        for (&seq, window) in &self.windows {
            if seq != ws.seq
                && let Some(native) = &window.window
            {
                native.request_redraw();
            }
        }
    }

    /// Submit coalesced background process scans at most every 200 ms and apply
    /// completed remote-context changes.
    ///
    /// When the worker's snapshot shows a new or changed detection, the
    /// pane's title becomes `format_remote_title(...)`. When the detection
    /// ends (SSH exited), the title from before the remote session is
    /// restored.
    fn poll_remote_contexts(&mut self, ws: &mut WindowState) {
        let Some(worker) = self.remote_scan_worker.as_ref() else {
            return;
        };
        let now = std::time::Instant::now();
        if let Some(due) = remote_poll_deferred_until(self.last_remote_poll, now) {
            self.remote_poll_due = Some(due);
            return;
        }
        self.last_remote_poll = Some(now);
        self.remote_poll_due = None;
        stage_remote_targets(
            &mut self.remote_targets_scratch,
            ws.mux.panes.values().map(remote_probe_target),
            self.windows
                .values()
                .flat_map(|window| window.mux.panes.values())
                .map(remote_probe_target),
        );
        worker.submit(self.remote_targets_scratch.clone());
    }

    fn apply_remote_scan_snapshot(&mut self, ws: &mut WindowState) {
        let Some(snapshot) = self
            .remote_scan_worker
            .as_ref()
            .and_then(kettle_remote::RemoteScanWorker::take_latest)
        else {
            return;
        };
        retain_live_remote_probes(
            &mut self.remote_applied,
            &mut self.remote_live_panes_scratch,
            ws.mux
                .panes
                .iter()
                .chain(
                    self.windows
                        .values()
                        .flat_map(|window| window.mux.panes.iter()),
                )
                .map(|(&pane_id, pane)| (pane_id, pane.term.child_pid())),
        );

        if update_window_remote_contexts(&snapshot, &mut self.remote_applied, ws) {
            ws.reuse_pane_snapshots_once = false;
            self.sync_window_title(ws);
            self.poll_title_event(ws);
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }

        // `ws` is checked out of this map. Temporarily take the remaining map
        // so title synchronization can borrow the rest of App without aliasing.
        let mut stored_windows = std::mem::take(&mut self.windows);
        for window in stored_windows.values_mut() {
            if update_window_remote_contexts(&snapshot, &mut self.remote_applied, window) {
                window.reuse_pane_snapshots_once = false;
                self.sync_window_title(window);
                self.poll_title_event(window);
                if let Some(handle) = &window.window {
                    handle.request_redraw();
                }
            }
        }
        self.windows = stored_windows;
    }

    /// The window numbered `seq`: `ws` or another.
    fn window_by_seq<'a>(
        &'a mut self,
        ws: &'a mut WindowState,
        seq: u64,
    ) -> Option<&'a mut WindowState> {
        if ws.seq == seq {
            Some(ws)
        } else {
            self.windows.get_mut(&seq)
        }
    }

    /// The one routing decision shared by real keyboard input and the
    /// `dispatch_keybind` control route: does this resolved chord belong to
    /// the terminal instead of the matched action? Only the non-macOS default
    /// `Alt+Arrow` focus chords are adaptive; they fall through when the mux
    /// has no visible pane in that direction, which includes every zoomed
    /// multi-pane tab because zoom collapses the layout to the focused pane.
    fn adaptive_focus_chord_falls_through(
        &self,
        ws: &WindowState,
        trigger: Trigger,
        action: &Action,
    ) -> bool {
        // A trigger the config binds itself, even to its default action, is
        // always Kettle's, as for every program key rule.
        let enabled = !cfg!(target_os = "macos") && !self.cfg.keybinds_declared.contains(&trigger);
        adaptive_alt_focus_direction(trigger, action, enabled).is_some_and(|(dx, dy)| {
            adaptive_alt_focus_falls_through(
                trigger,
                action,
                true,
                ws.mux.pane_in_direction(self.area(ws), dx, dy).is_some(),
            )
        })
    }

    /// The routing decision the real keyboard and `dispatch_keybind` share: a
    /// matched chord goes to the terminal when it is an adaptive focus chord
    /// with nowhere to go, or when a program that owns the keyboard also uses
    /// it (see [`program_key_rule`]). The search bar keeps only the first
    /// rule: while it has the keyboard, the program cannot get the key, so a
    /// Kettle action is still better than nothing.
    fn chord_falls_through(&self, ws: &WindowState, trigger: Trigger, action: &Action) -> bool {
        self.adaptive_focus_chord_falls_through(ws, trigger, action)
            || self.chord_goes_to_program(ws, trigger, action)
    }

    /// Whether a default chord that the focused pane's program also uses goes
    /// to that program: only default (trigger, action) pairs with a
    /// [`ProgramKeyRule`], only while `keybind-yield` is on, and only while a
    /// program, not an idle shell prompt, owns the pane's keyboard. Reads the
    /// pane's terminal only after the cheap pair match succeeds.
    fn chord_goes_to_program(&self, ws: &WindowState, trigger: Trigger, action: &Action) -> bool {
        let Some(rule) =
            configured_program_key_rule(&self.cfg, trigger, action, cfg!(target_os = "macos"))
        else {
            return false;
        };
        // Broadcast writes the key to every target in that target's own
        // keyboard mode, but only the focused pane is asked here, so a
        // broadcast chord stays Kettle's.
        if ws.mux.is_broadcast_on() {
            return false;
        }
        let Some(pane) = ws.mux.active_focus().and_then(|id| ws.mux.panes.get(&id)) else {
            return false;
        };
        // A pane that cannot take input would drop the key.
        if pane.read_only || pane.pty_input_failed() {
            return false;
        }
        let claims = pane.term.keyboard_claims();
        if !claims.program_owns_keyboard() {
            return false;
        }
        let kettle = KettleActionState {
            tabs: ws.mux.tabs.len(),
            current_tab: ws.mux.active,
            selection_exists: matches!(action, Action::SelectToTop | Action::SelectToBottom)
                && pane
                    .term
                    .term
                    .lock()
                    .ok()
                    .is_some_and(|t| t.selection.is_some()),
            focus_target_exists: self.focus_target_exists(ws, action),
            chord_is_distinct: rule == ProgramKeyRule::WhenKettleIdleAndDistinct
                && pane
                    .term
                    .term
                    .lock()
                    .ok()
                    .is_some_and(|t| chord_reaches_program_distinctly(trigger, *t.mode())),
        };
        program_gets_chord(rule, action, &claims, &kettle)
    }

    /// Whether a focus action has a visible pane to move to. A zoomed tab
    /// shows only its focused pane, so it has none in any direction, as a
    /// single-pane tab has none.
    fn focus_target_exists(&self, ws: &WindowState, action: &Action) -> bool {
        let direction = match action {
            Action::FocusUp => (0, -1),
            Action::FocusDown => (0, 1),
            Action::FocusLeft => (-1, 0),
            Action::FocusRight => (1, 0),
            Action::FocusNext | Action::FocusPrev => {
                return ws.mux.leaves(ws.mux.active, self.area(ws)).len() > 1;
            }
            _ => return false,
        };
        ws.mux
            .pane_in_direction(self.area(ws), direction.0, direction.1)
            .is_some()
    }
}

/// The modal text fields `dispatch_ui_key` can drive, in the precedence the
/// real `KeyboardInput` arm applies.
///
/// An enum rather than strings so the dispatch `match` is exhaustive: adding a
/// variant without routing it fails to compile. That is *not* a guarantee that
/// a new modal added to the key handler reaches this enum at all — nothing
/// forces that, which is what
/// `the_control_plane_modal_order_matches_the_real_key_handler` is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TextModal {
    Confirm,
    Hint,
    Palette,
    ThemePicker,
    SettingsText,
    Settings,
    LayoutPicker,
    Ssh,
    TitleEdit,
    Search,
    Preview,
}

/// Editor text and toggles that an open search bar brings along when it
/// follows focus to another pane (`App::retarget_search_to_focus`). A fresh
/// open fills it from the pane's remembered query and the configured defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchCarryOver {
    query: String,
    wrap: bool,
    case_mode: kettle_config::SearchCaseSensitivity,
    invert: bool,
    focused_control: kettle_render::SearchControl,
}

/// The pane an open bar must move to, if any: `None` while the bar already
/// targets the focused pane or nothing is focused.
fn search_retarget(target_pane: Option<u64>, focus: Option<u64>) -> Option<u64> {
    focus.filter(|pane| Some(*pane) != target_pane)
}

/// The bar's state for `pane_id`, scanning from `display_offset`. A non-empty
/// query starts searching immediately; the anchor is derived by the caller
/// once the lane's resize has settled.
fn fresh_search_state(
    pane_id: u64,
    carried: SearchCarryOver,
    display_offset: usize,
    now: std::time::Instant,
) -> crate::search_input::SearchState {
    let mut search = crate::search_input::SearchState {
        open: true,
        target_pane: Some(pane_id),
        editor: crate::search_input::SearchEditor::from_text(
            carried.query,
            kettle_core::MAX_SEARCH_QUERY_BYTES,
        ),
        wrap: carried.wrap,
        case_mode: carried.case_mode,
        invert: carried.invert,
        focused_control: carried.focused_control,
        anchor: None,
        pre_open_display_offset: Some(display_offset),
        ..crate::search_input::SearchState::default()
    };
    if !search.query().is_empty() {
        search.note_edit(now);
    }
    search
}

impl TextModal {
    /// Stable wire spelling, reported by `dispatch_ui_key`.
    const fn as_str(self) -> &'static str {
        match self {
            Self::Confirm => "confirm",
            Self::Hint => "hint_mode",
            Self::Palette => "palette",
            Self::ThemePicker => "theme_picker",
            Self::SettingsText => "settings_text",
            Self::Settings => "settings",
            Self::LayoutPicker => "layout_picker",
            Self::Ssh => "ssh",
            Self::TitleEdit => "title_edit",
            Self::Search => "search",
            Self::Preview => "preview",
        }
    }
}

/// The supported text surface owned by the same modal as a real key event.
fn open_text_modal(ws: &WindowState) -> Option<TextModal> {
    Some(match top_modal(ws)? {
        KeyModal::ConfirmDialog => TextModal::Confirm,
        KeyModal::ContextMenu | KeyModal::ViMode => return None,
        KeyModal::Hint => TextModal::Hint,
        KeyModal::Palette => TextModal::Palette,
        KeyModal::ThemePicker => TextModal::ThemePicker,
        KeyModal::SettingsText => TextModal::SettingsText,
        KeyModal::SettingsCapture | KeyModal::Settings => TextModal::Settings,
        KeyModal::LayoutPicker => TextModal::LayoutPicker,
        KeyModal::Ssh => TextModal::Ssh,
        KeyModal::TitleEdit => TextModal::TitleEdit,
        KeyModal::Search => TextModal::Search,
        KeyModal::Preview => TextModal::Preview,
    })
}

impl App {}

/// Terminator parity: remap encoded Backspace/Delete bytes per the user's
/// `backspace-binding`/`delete-binding`. `send_keys` uses it too, so it honors
/// the same remap as GUI keystrokes (the "same path as a human key press"
/// contract).
///
/// The binding only ever replaces the UNMODIFIED encoding, and the test for
/// that is the encoded bytes themselves, not the modifier state. Delete's
/// `CSI 3 ~` form carries a modifier parameter for *shift, alt and control
/// alike* (Super has no legacy representation and is rejected before this
/// point). With `delete-binding` defaulting to `EscapeSequence`, remapping a
/// modified Delete would rewrite it to the plain `CSI 3 ~`, so `Ctrl+Delete`
/// would equal `Delete` and readline's `kill-word` and every `<C-Del>` /
/// `<S-Del>` / `<M-Del>` editor mapping would delete one character instead.
///
/// Comparing against the plain form cannot drift from the encoder, and it is
/// right for the cases modifier logic keeps missing: a `modifyOtherKeys`
/// level-1 `Shift+Backspace` (`CSI 27;2;8~`) and any kitty-protocol encoding
/// are left alone, because an application that negotiated a precise encoding
/// must not have it overwritten by a legacy remap.
fn apply_bs_del_binding(cfg: &Config, key: &Key, bytes: Vec<u8>) -> Vec<u8> {
    let Key::Named(named) = key else {
        return bytes;
    };
    use kettle_config::{BackspaceBinding, DeleteBinding};
    /// What `input::encode` emits for an unmodified Backspace.
    const PLAIN_BACKSPACE: &[u8] = &[0x7f];
    /// What `input::encode` emits for an unmodified Delete.
    const PLAIN_DELETE: &[u8] = b"\x1b[3~";
    if *named == NamedKey::Backspace && bytes == PLAIN_BACKSPACE {
        match cfg.backspace_binding {
            BackspaceBinding::AsciiDel => vec![0x7f],
            BackspaceBinding::ControlH => vec![0x08],
            BackspaceBinding::EscapeSequence => b"\x1b[3~".to_vec(),
            BackspaceBinding::Automatic => bytes,
        }
    } else if *named == NamedKey::Delete && bytes == PLAIN_DELETE {
        match cfg.delete_binding {
            DeleteBinding::AsciiDel => vec![0x7f],
            DeleteBinding::ControlH => vec![0x08],
            DeleteBinding::EscapeSequence => b"\x1b[3~".to_vec(),
            DeleteBinding::Automatic => bytes,
        }
    } else {
        bytes
    }
}

fn encode_key_event_for_mode(
    cfg: &Config,
    event: &winit::event::KeyEvent,
    mods: ModifiersState,
    mode: kettle_core::TermMode,
) -> Option<Vec<u8>> {
    let mut bytes = input::encode_key_event(event, mods, mode)?;
    if !input::uses_kitty_sequence(event, mods, mode) {
        bytes = apply_bs_del_binding(cfg, &event.logical_key, bytes);
    }
    Some(bytes)
}

fn key_is_modified_enter(key: &Key, mods: ModifiersState) -> bool {
    matches!(key, Key::Named(NamedKey::Enter))
        && (mods.shift_key() || mods.control_key() || mods.alt_key() || mods.super_key())
}

fn parse_ctl_send_key_batch(
    keys: &[serde_json::Value],
) -> std::result::Result<Vec<(String, ModifiersState, Key)>, String> {
    if keys.is_empty() || keys.len() > MAX_CTL_SEND_KEYS {
        return Err(format!(
            "'keys' must contain 1..={MAX_CTL_SEND_KEYS} entries"
        ));
    }
    let mut parsed = Vec::with_capacity(keys.len());
    for value in keys {
        let Some(token) = value.as_str() else {
            return Err("non-string entry in 'keys'".into());
        };
        if token.is_empty() || token.len() > MAX_CTL_SEND_KEY_TOKEN_BYTES {
            return Err(format!(
                "key tokens must be 1..={MAX_CTL_SEND_KEY_TOKEN_BYTES} bytes"
            ));
        }
        let Some((mods, key)) = parse_send_key(token) else {
            return Err(format!("unrecognized key token '{token}'"));
        };
        parsed.push((token.to_owned(), mods, key));
    }
    Ok(parsed)
}

fn extend_ctl_send_key_bytes(
    bytes: &mut Vec<u8>,
    encoded: &[u8],
) -> std::result::Result<(), String> {
    if encoded.len() > MAX_CTL_SEND_KEY_BYTES.saturating_sub(bytes.len()) {
        return Err(format!(
            "encoded key sequence exceeds {MAX_CTL_SEND_KEY_BYTES} bytes"
        ));
    }
    bytes.extend_from_slice(encoded);
    Ok(())
}

/// Parse one `send_keys` token (`"escape"`, `"ctrl+c"`, `"shift+tab"`,
/// `"f5"`, `"alt+enter"`, a bare character like `"G"`) into the
/// `(mods, key)` pair the GUI's PTY encoder (`input::encode_key_press`)
/// consumes. Same `+`-separated grammar and modifier aliases as config
/// keybind triggers (`parse_trigger`), plus names the keybind grammar lacks
/// (escape, insert, comma). Character case is preserved (`"G"` sends `G`,
/// like a human holding Shift). `None` on an unrecognized token, so the
/// caller can name the bad token instead of sending wrong bytes. Pure
/// (unit-tested).
fn parse_send_key(token: &str) -> Option<(ModifiersState, Key)> {
    parse_key_token(token)
}

/// Parse a Kettle-owned UI key. Application chrome can consume Super/Command
/// character chords directly (for example Command+A on macOS).
fn parse_ui_key(token: &str) -> Option<(ModifiersState, Key)> {
    let (mods, key) = parse_key_token(token)?;
    let key = if key == Key::Named(NamedKey::Space) {
        Key::Character(" ".into())
    } else if matches!(
        &key,
        Key::Named(
            NamedKey::Escape
                | NamedKey::Enter
                | NamedKey::Tab
                | NamedKey::Backspace
                | NamedKey::Delete
                | NamedKey::ArrowUp
                | NamedKey::ArrowDown
                | NamedKey::ArrowLeft
                | NamedKey::ArrowRight
                | NamedKey::Home
                | NamedKey::End
                | NamedKey::F3
        ) | Key::Character(_)
    ) {
        key
    } else {
        return None;
    };
    Some((mods, key))
}

fn parse_key_token(token: &str) -> Option<(ModifiersState, Key)> {
    let parts: Vec<&str> = token.split('+').collect();
    let last = parts.len().checked_sub(1)?;
    let mut mods = ModifiersState::empty();
    let mut key: Option<Key> = None;
    for (i, part) in parts.iter().enumerate() {
        let raw = part.trim();
        let lower = raw.to_ascii_lowercase();
        match lower.as_str() {
            "ctrl" | "control" => mods |= ModifiersState::CONTROL,
            "alt" | "opt" | "option" => mods |= ModifiersState::ALT,
            "shift" => mods |= ModifiersState::SHIFT,
            "super" | "cmd" | "command" | "win" | "windows" | "meta" | "logo" => {
                mods |= ModifiersState::SUPER;
            }
            _ => {
                // Only the LAST slot may be a key (same rule as parse_trigger:
                // a typo'd modifier must fail loudly, not bind a plain key).
                if i != last {
                    return None;
                }
                // Named characters first, since a literal `,` can't survive the
                // CLI's comma-split `--keys` and `+` is every client's chord
                // separator. Mirrors `parse_key`'s plus/minus/equal aliases.
                match lower.as_str() {
                    "plus" => {
                        key = Some(Key::Character("+".into()));
                        continue;
                    }
                    "comma" => {
                        key = Some(Key::Character(",".into()));
                        continue;
                    }
                    "minus" => {
                        key = Some(Key::Character("-".into()));
                        continue;
                    }
                    "equal" => {
                        key = Some(Key::Character("=".into()));
                        continue;
                    }
                    _ => {}
                }
                let named = match lower.as_str() {
                    "escape" | "esc" => Some(NamedKey::Escape),
                    "enter" | "return" => Some(NamedKey::Enter),
                    "tab" => Some(NamedKey::Tab),
                    "backspace" | "bs" => Some(NamedKey::Backspace),
                    "delete" | "del" => Some(NamedKey::Delete),
                    "insert" | "ins" => Some(NamedKey::Insert),
                    "space" => Some(NamedKey::Space),
                    "up" => Some(NamedKey::ArrowUp),
                    "down" => Some(NamedKey::ArrowDown),
                    "left" => Some(NamedKey::ArrowLeft),
                    "right" => Some(NamedKey::ArrowRight),
                    "home" => Some(NamedKey::Home),
                    "end" => Some(NamedKey::End),
                    "page_up" | "pageup" | "prior" => Some(NamedKey::PageUp),
                    "page_down" | "pagedown" | "next" => Some(NamedKey::PageDown),
                    "f1" => Some(NamedKey::F1),
                    "f2" => Some(NamedKey::F2),
                    "f3" => Some(NamedKey::F3),
                    "f4" => Some(NamedKey::F4),
                    "f5" => Some(NamedKey::F5),
                    "f6" => Some(NamedKey::F6),
                    "f7" => Some(NamedKey::F7),
                    "f8" => Some(NamedKey::F8),
                    "f9" => Some(NamedKey::F9),
                    "f10" => Some(NamedKey::F10),
                    "f11" => Some(NamedKey::F11),
                    "f12" => Some(NamedKey::F12),
                    _ => None,
                };
                key = Some(match named {
                    Some(n) => Key::Named(n),
                    None => {
                        // A single character, case preserved.
                        let mut ch = raw.chars();
                        let mut c = ch.next()?;
                        if ch.next().is_some() {
                            return None;
                        }
                        // A bare uppercase ASCII token represents the same
                        // logical action as a GUI Shift+letter press. Preserve
                        // that inferred modifier so negotiated Kitty CSI-u can
                        // report the unshifted primary key and shifted
                        // alternate key instead of misreporting a Caps-Lock
                        // style unmodified uppercase key.
                        if c.is_ascii_uppercase() {
                            mods |= ModifiersState::SHIFT;
                        }
                        // Keep SHIFT on alphabetic chords while normalizing
                        // their logical character to uppercase: the legacy
                        // encoder still emits the expected byte, while Kitty
                        // CSI-u needs the live modifier bit to distinguish
                        // Ctrl+C from Ctrl+Shift+C.
                        if mods.contains(ModifiersState::SHIFT) && c.is_ascii_alphabetic() {
                            c = c.to_ascii_uppercase();
                        }
                        Key::Character(c.to_string().into())
                    }
                });
            }
        }
    }
    key.map(|k| (mods, k))
}

fn push_unique_trigger(out: &mut Vec<Trigger>, trigger: Trigger) {
    if !out.contains(&trigger) {
        out.push(trigger);
    }
}

fn physical_keybind_chars(physical: &PhysicalKey, mods: ModifiersState) -> &'static [char] {
    match physical {
        PhysicalKey::Code(KeyCode::Equal) => {
            if mods.shift_key() {
                &['=', '+']
            } else {
                &['=']
            }
        }
        PhysicalKey::Code(KeyCode::Minus) => {
            if mods.shift_key() {
                &['-', '_']
            } else {
                &['-']
            }
        }
        // US punctuation keys. A binding written with the unshifted
        // character, as imported Terminator accelerators are
        // (`<Control><Shift>comma`), must match the shifted character the
        // keyboard produces with Shift held.
        PhysicalKey::Code(KeyCode::Comma) => shifted(mods, &[','], &[',', '<']),
        PhysicalKey::Code(KeyCode::Period) => shifted(mods, &['.'], &['.', '>']),
        PhysicalKey::Code(KeyCode::Slash) => shifted(mods, &['/'], &['/', '?']),
        PhysicalKey::Code(KeyCode::Semicolon) => shifted(mods, &[';'], &[';', ':']),
        PhysicalKey::Code(KeyCode::Quote) => shifted(mods, &['\''], &['\'', '"']),
        PhysicalKey::Code(KeyCode::BracketLeft) => shifted(mods, &['['], &['[', '{']),
        PhysicalKey::Code(KeyCode::BracketRight) => shifted(mods, &[']'], &[']', '}']),
        PhysicalKey::Code(KeyCode::Backslash) => shifted(mods, &['\\'], &['\\', '|']),
        PhysicalKey::Code(KeyCode::Backquote) => shifted(mods, &['`'], &['`', '~']),
        PhysicalKey::Code(KeyCode::NumpadAdd) => &['+'],
        PhysicalKey::Code(KeyCode::NumpadSubtract) => &['-'],
        PhysicalKey::Code(KeyCode::NumpadEqual) => &['='],
        PhysicalKey::Code(KeyCode::Digit0) | PhysicalKey::Code(KeyCode::Numpad0) => &['0'],
        _ => &[],
    }
}

fn shifted(
    mods: ModifiersState,
    plain: &'static [char],
    with_shift: &'static [char],
) -> &'static [char] {
    if mods.shift_key() { with_shift } else { plain }
}

fn keybind_candidates(
    logical_key: Option<&Key>,
    physical_key: Option<&PhysicalKey>,
    mods: ModifiersState,
) -> Vec<Trigger> {
    let mut out = Vec::new();
    let kmods = to_mods(mods);
    if let Some(k) = logical_key.and_then(to_kkey) {
        push_unique_trigger(&mut out, Trigger::new(kmods, k));
    }
    if let Some(physical) = physical_key {
        let aliases = physical_keybind_chars(physical, mods);
        if physical_aliases_apply(logical_key, aliases) {
            for c in aliases {
                push_unique_trigger(&mut out, Trigger::new(kmods, KKey::Char(*c)));
            }
        }
    }
    out
}

/// Whether a key's US characters may stand in for what the layout produced.
/// Only where the layout gives the key no ASCII character of its own (a
/// non-Latin layout, or AZERTY's `à` on the 0 key) or produced one of those
/// same characters at another shift level. On Dvorak, Ctrl+W is the US comma
/// key: aliasing it to Ctrl+, would open Settings instead of reaching the
/// shell.
fn physical_aliases_apply(logical_key: Option<&Key>, aliases: &[char]) -> bool {
    let Some(Key::Character(text)) = logical_key else {
        return true;
    };
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) if c.is_ascii() => aliases.contains(&c),
        _ => true,
    }
}

/// Whether the search bar uses a key chord itself, on macOS or elsewhere.
/// Typed text is decided separately (`modal_input::accept_text`), and any
/// other chord goes on to Kettle's shortcuts.
///
/// The bar keeps what a native find field keeps: Escape, Enter, Tab and F3
/// without Ctrl, Cmd or Alt; Backspace and Delete, alone or with the word
/// modifier (and Cmd on macOS); the caret keys with Shift
/// and the word modifier (Option on macOS, Ctrl elsewhere), plus Cmd on macOS
/// for the line ends; the shortcut key (Cmd on macOS, Ctrl elsewhere) with
/// A, C, V, X, G and Z; and on macOS the Ctrl text-field keys. `Cmd+Opt+Arrow`
/// and Linux's `Alt+Arrow` move focus between splits, `Ctrl+Home` / `Ctrl+End`
/// scroll the terminal, and on Linux `Ctrl+Shift+A` and `Ctrl+Shift+X` keep
/// their split and zoom meanings.
fn search_bar_claims(
    key: &Key,
    mods: ModifiersState,
    macos: bool,
    editor_focused: bool,
    vim_menu_nav: bool,
) -> bool {
    let (ctrl, alt, sup, shift) = (
        mods.control_key(),
        mods.alt_key(),
        mods.super_key(),
        mods.shift_key(),
    );
    let shortcut_only = if macos {
        sup && !ctrl && !alt
    } else {
        ctrl && !sup && !alt
    };
    let no_chord = !ctrl && !sup && !alt;
    let caret_chord = if macos {
        !ctrl && !(sup && alt)
    } else {
        !alt && !sup
    };
    match key {
        Key::Named(NamedKey::Escape) => true,
        // Deletion takes the word modifier, and Cmd for the line ends on
        // macOS; any other chord is left for a shortcut.
        Key::Named(NamedKey::Backspace | NamedKey::Delete) => {
            if macos {
                !ctrl
            } else {
                !alt && !sup
            }
        }
        Key::Named(NamedKey::Enter | NamedKey::F3 | NamedKey::Tab) => no_chord,
        Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight) => editor_focused && caret_chord,
        Key::Named(NamedKey::Home | NamedKey::End) => editor_focused && no_chord,
        Key::Named(NamedKey::Space) => !editor_focused && no_chord,
        Key::Character(s) => {
            let lower = s.to_ascii_lowercase();
            let vim = vim_menu_nav && ctrl && !alt && matches!(s.as_str(), "j" | "k" | "n" | "p");
            let shortcut = shortcut_only
                && match lower.as_str() {
                    "g" | "z" | "c" | "v" => true,
                    "a" | "x" => macos || !shift,
                    _ => false,
                };
            let text_field = macos
                && editor_focused
                && ctrl
                && !sup
                && !alt
                && !shift
                && matches!(lower.as_str(), "a" | "e" | "b" | "f" | "d" | "h" | "k");
            let press = s == " " && !editor_focused && no_chord;
            vim || shortcut || text_field || press
        }
        _ => false,
    }
}

/// The text a key types into the query, from what the shared modal rule
/// accepted (`accepted`). An Alt chord that is bound to a shortcut runs the
/// shortcut instead: Linux's `Alt+1` switches to tab 1 rather than typing `1`.
/// On Windows AltGr arrives as Ctrl+Alt (`altgr_is_ctrl_alt`), and it keeps
/// typing the character it composes.
fn search_bar_text(
    accepted: Option<&str>,
    mods: ModifiersState,
    bound: bool,
    altgr_is_ctrl_alt: bool,
) -> Option<&str> {
    let altgr = altgr_is_ctrl_alt && mods.control_key();
    accepted.filter(|_| !(mods.alt_key() && !altgr && bound))
}

/// How a Kettle shortcut behaves while the search bar has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchBarShortcut {
    /// It types into the terminal, which does not have the keyboard.
    Typing,
    /// It copies from the query, or the grid selection under the bar.
    Copy,
    /// It pastes into the query.
    Paste,
    /// It runs as usual.
    Run,
}

fn search_bar_shortcut_kind(action: &Action) -> SearchBarShortcut {
    match action {
        Action::SendText(_)
        | Action::SendNewline
        | Action::InsertPaneNumber
        | Action::InsertPanePadded
        | Action::InsertPaneName
        | Action::PastePrimary => SearchBarShortcut::Typing,
        Action::Copy => SearchBarShortcut::Copy,
        Action::Paste => SearchBarShortcut::Paste,
        _ => SearchBarShortcut::Run,
    }
}

/// Whether a held key's repeat is dropped in the search bar. A focused
/// toggle or Close acts once per press, as a native button does, so holding
/// Space or Enter does not flip a setting (and rewrite the config) at the
/// repeat rate. Previous and Next repeat, like Enter held in the query.
fn search_button_ignores_repeat(focused: kettle_render::SearchControl, key: &Key) -> bool {
    let activates = match key {
        Key::Named(NamedKey::Space | NamedKey::Enter) => true,
        Key::Character(text) => text == " ",
        _ => false,
    };
    activates
        && !matches!(
            focused,
            kettle_render::SearchControl::Editor
                | kettle_render::SearchControl::Previous
                | kettle_render::SearchControl::Next
        )
}

/// Drop the repeats of a key that closed a Kettle modal. They belong to the
/// press the modal consumed, not to the terminal it uncovered; its release is
/// already suppressed with that press. The key's release, or a fresh press of
/// it, ends the suppression. Returns whether to drop this event.
fn drop_closing_key_repeat(
    closing: &mut HashSet<PhysicalKey>,
    physical_key: PhysicalKey,
    state: ElementState,
    repeat: bool,
) -> bool {
    if !closing.contains(&physical_key) {
        return false;
    }
    if state == ElementState::Pressed && repeat {
        return true;
    }
    closing.remove(&physical_key);
    false
}

/// The Kettle modal that owns the keyboard, in the keyboard path's dispatch
/// order (the first open one gets the key).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum KeyModal {
    ConfirmDialog,
    ContextMenu,
    ViMode,
    Hint,
    Palette,
    ThemePicker,
    SettingsText,
    SettingsCapture,
    Settings,
    LayoutPicker,
    Ssh,
    TitleEdit,
    Search,
    /// A preview lane holding the keyboard (`focus_preview`).
    Preview,
}

fn top_modal(ws: &WindowState) -> Option<KeyModal> {
    Some(if ws.confirm_dialog.is_some() {
        KeyModal::ConfirmDialog
    } else if ws.context_menu.is_some() {
        KeyModal::ContextMenu
    } else if ws.vi_mode.is_some() {
        KeyModal::ViMode
    } else if ws.hint_state.is_some() {
        KeyModal::Hint
    } else if ws.palette_input.is_some() {
        KeyModal::Palette
    } else if ws.theme_picker_input.is_some() {
        KeyModal::ThemePicker
    } else if ws.settings_nav.is_some() && ws.settings_text_edit.is_some() {
        KeyModal::SettingsText
    } else if settings_capture_active(ws) {
        KeyModal::SettingsCapture
    } else if ws.settings_nav.is_some() {
        KeyModal::Settings
    } else if ws.layout_picker_input.is_some() {
        KeyModal::LayoutPicker
    } else if ws.ssh_input.is_some() {
        KeyModal::Ssh
    } else if ws.editing_title.is_some() {
        KeyModal::TitleEdit
    } else if ws.search.open {
        KeyModal::Search
    } else if ws.preview_focus.is_some() {
        KeyModal::Preview
    } else {
        return None;
    })
}

/// Record `key` as the one that closed or replaced the modal it was pressed
/// in, so its auto-repeats are dropped (`drop_closing_key_repeat`) instead of
/// reaching whatever the modal uncovered: the terminal, the parent Settings
/// under a closed text field, or the layout picker the palette just opened.
/// The modal's input-method composition ends with it, for the same reason.
fn remember_closing_key(ws: &mut WindowState, key_modal: Option<KeyModal>, key: PhysicalKey) {
    if top_modal(ws) != key_modal {
        ws.closing_keys.insert(key);
        end_modal_composition(ws);
    }
}

/// A Kettle modal whose keys do not all repeat (see `modal_repeat_is_inert`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModalRepeat {
    ConfirmDialog,
    /// `vim_nav`: `vim-menu-nav` is on, so `j`/`k` and Ctrl+D/U move.
    ContextMenu {
        vim_nav: bool,
    },
    ViMode,
    Hint,
    Palette,
    /// `steps`: the focused row is a number, which Left/Right step. On a
    /// toggle or a choice they flip or cycle it and persist each step.
    Settings {
        steps: bool,
    },
}

/// Whether an auto-repeat of `key` must do nothing in `modal`. Keys that
/// activate, toggle or pick act once per press: holding the Enter that opened
/// a submenu would otherwise activate its first row, and holding Space on a
/// Settings toggle would flip it at the repeat rate. Keys that move keep
/// repeating.
fn modal_repeat_is_inert(modal: ModalRepeat, repeat: bool, key: &Key, ctrl: bool) -> bool {
    if !repeat {
        return false;
    }
    let moves = |key: &Key| {
        matches!(
            key,
            Key::Named(
                NamedKey::ArrowUp
                    | NamedKey::ArrowDown
                    | NamedKey::ArrowLeft
                    | NamedKey::ArrowRight
                    | NamedKey::PageUp
                    | NamedKey::PageDown
                    | NamedKey::Tab
            )
        )
    };
    let letter = |key: &Key, letters: &[&str]| matches!(key, Key::Character(c) if letters.iter().any(|l| c.eq_ignore_ascii_case(l)));
    match modal {
        // Rows are also picked by mnemonic letters and vim `l`; only the
        // highlight moves repeat: arrows, and with vim-menu-nav `j`/`k` and
        // Ctrl+D/U half pages. Without it, `j` and `k` are mnemonics.
        ModalRepeat::ContextMenu { vim_nav } => {
            let vim_move = vim_nav
                && if ctrl {
                    letter(key, &["d", "u"])
                } else {
                    letter(key, &["j", "k"])
                };
            !(moves(key) || vim_move)
        }
        // A dialog's letters and Enter/Space answer it; focus moves repeat.
        ModalRepeat::ConfirmDialog => !moves(key),
        // The query is typed text; only Enter runs the selection.
        ModalRepeat::Palette => matches!(key, Key::Named(NamedKey::Enter)),
        // Enter and Space activate or toggle the focused row. Left/Right (and
        // vim `h`/`l`) step a number and repeat; on a toggle or a choice they
        // flip or cycle it, persisting each step, so act once per press.
        ModalRepeat::Settings { steps } => {
            matches!(key, Key::Named(NamedKey::Enter | NamedKey::Space))
                || (!steps
                    && (matches!(key, Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight))
                        || letter(key, &["h", "l"])))
        }
        // `v` toggles visual mode; motions repeat.
        ModalRepeat::ViMode => letter(key, &["v"]),
        // Label letters only pick a target; nothing in hint mode moves.
        ModalRepeat::Hint => true,
    }
}

/// Whether the Settings overlay is waiting for a keybind chord.
fn settings_capture_active(ws: &WindowState) -> bool {
    ws.settings_nav.as_ref().is_some_and(|nav| nav.capturing)
}

/// Whether an action flips a state, so auto-repeat of its held chord must not
/// run it again: the chord would otherwise flip the state back and forth for
/// as long as it is held, and leave it wherever the last repeat fell. Such a
/// repeat is still consumed, so it never reaches the program.
fn toggles_once_per_press(action: &Action) -> bool {
    matches!(
        action,
        Action::ToggleViMode
            | Action::ToggleZoom
            | Action::ScaledZoom
            | Action::ToggleFullscreen
            | Action::ToggleBroadcastAll
            | Action::ToggleBroadcastGroup
            | Action::ToggleBroadcastOff
            | Action::ToggleBroadcastWindow
            | Action::ToggleCopyOnSelect
            | Action::ToggleCursorBlink
            | Action::ToggleMouseHide
            | Action::TogglePaneReadOnly
            | Action::ToggleScrollbar
            | Action::ToggleLightDark
            | Action::ToggleSessionLog
            | Action::ToggleGroupAll
            | Action::ToggleGroupTab
            | Action::ToggleGroupWindow
            | Action::ToggleWindowVisibility
            | Action::OpenMediaShelf
    )
}

/// Whether a key press is bound to toggling vi mode. Vi mode takes every key
/// before the keybind table is consulted, so without this check the chord
/// that entered it (`Ctrl+Shift+Space` by default) could not leave it.
fn is_vi_mode_toggle_chord(
    bindings: &Bindings,
    logical_key: &Key,
    physical_key: &PhysicalKey,
    mods: ModifiersState,
) -> bool {
    matches!(
        resolve_keybind_action(bindings, Some(logical_key), Some(physical_key), mods),
        Some((_, Action::ToggleViMode))
    )
}

fn resolve_keybind_action(
    bindings: &Bindings,
    logical_key: Option<&Key>,
    physical_key: Option<&PhysicalKey>,
    mods: ModifiersState,
) -> Option<(Trigger, Action)> {
    keybind_candidates(logical_key, physical_key, mods)
        .into_iter()
        .find_map(|trigger| {
            bindings
                .get(&trigger)
                .cloned()
                .map(|action| (trigger, action))
        })
}

/// The non-macOS default `Alt+Arrow` focus chords are adaptive at the edge of
/// the visible split tree. When a visible neighbour exists, the keybind owns
/// the chord and moves focus. When no visible pane exists in that direction,
/// the original key event falls through to the PTY, preserving application
/// bindings such as Codex's `Alt+Left`/`Alt+Right` word motion and `Alt+Up`
/// previous-message editor.
///
/// "Visible" is the operative word: a zoomed pane (`Ctrl+Shift+X`, or the
/// `scaled_zoom` action) hides its siblings, so directional focus has nowhere to go
/// and the chord belongs to the program until the zoom is released. A Kettle
/// no-op there would swallow the press and its release for nothing.
///
/// Match the trigger and action as a pair. A user who deliberately binds
/// `Alt+Up` to some other action must get that action, not an implicit terminal
/// fallback, and actions invoked from menus or automation have no physical
/// chord to reinterpret. macOS keeps its explicit focus chords
/// (`Cmd+Opt+Arrow` and `Ctrl+Cmd+Arrow`) and leaves Option+Arrow entirely to
/// the terminal.
fn adaptive_alt_focus_direction(
    trigger: Trigger,
    action: &Action,
    enabled: bool,
) -> Option<(i32, i32)> {
    if !enabled || trigger.mods != Mods::ALT {
        return None;
    }
    match (trigger.key, action) {
        (KKey::Up, Action::FocusUp) => Some((0, -1)),
        (KKey::Down, Action::FocusDown) => Some((0, 1)),
        (KKey::Left, Action::FocusLeft) => Some((-1, 0)),
        (KKey::Right, Action::FocusRight) => Some((1, 0)),
        _ => None,
    }
}

/// Whether an adaptive focus chord belongs to the terminal rather than to
/// Kettle. `visible_neighbor_exists` is the mux's answer for the chord's
/// direction; it is already `false` while zoom collapses the layout to the
/// focused pane, so no separate zoom flag is consulted.
fn adaptive_alt_focus_falls_through(
    trigger: Trigger,
    action: &Action,
    enabled: bool,
    visible_neighbor_exists: bool,
) -> bool {
    !visible_neighbor_exists && adaptive_alt_focus_direction(trigger, action, enabled).is_some()
}

/// How a default chord shares its key with a program that owns the keyboard
/// (see `chord_goes_to_program`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProgramKeyRule {
    /// The program always gets it: Shift+Arrow split resize, which Codex
    /// (Shift+Left answers a queued question), Claude Code, Neovim, fish and
    /// every text field use too.
    ProgramFirst,
    /// The program gets it only when Kettle's action would do nothing (no
    /// prompt to jump to, nothing to scroll, one tab, no selection to extend)
    /// and the view is not scrolled into history. Copy has no rule: Codex
    /// reads Ctrl+Shift+C as Ctrl+C, which would discard a draft.
    WhenKettleIdle,
    /// As `WhenKettleIdle`, for pane-focus chords that have nowhere to go (a
    /// zoomed or single-pane tab, or no pane on that side), and only while the
    /// pane's negotiated keyboard mode tells the chord apart. Sent in plain
    /// legacy mode, Ctrl+Shift+N would arrive as Ctrl+N, a different key, and
    /// a Command chord would arrive as nothing at all.
    WhenKettleIdleAndDistinct,
}

/// The rule for this chord under the user's config: none with
/// `keybind-yield = off`, and none for a trigger the config binds itself, even
/// to its default action, so a chord the user bound is always Kettle's.
fn configured_program_key_rule(
    cfg: &kettle_config::Config,
    trigger: Trigger,
    action: &Action,
    macos: bool,
) -> Option<ProgramKeyRule> {
    if cfg.keybind_yield == kettle_config::KeybindYield::Off
        || cfg.keybinds_declared.contains(&trigger)
    {
        return None;
    }
    program_key_rule(trigger, action, macos)
}

/// The default chords a program that owns the keyboard can have, matched as
/// exact (trigger, action) pairs, as `adaptive_alt_focus_direction` does: a
/// chord the user binds to anything, or an action the user moves to another
/// chord, is always Kettle's.
fn program_key_rule(trigger: Trigger, action: &Action, macos: bool) -> Option<ProgramKeyRule> {
    use KKey::*;
    use ProgramKeyRule::*;

    let shift = Mods::SHIFT;
    let ctrl = Mods::CTRL;
    let ctrl_shift = Mods::CTRL | Mods::SHIFT;
    let rule = match (trigger.mods, trigger.key, action) {
        (m, Up, Action::ResizeUp)
        | (m, Down, Action::ResizeDown)
        | (m, Left, Action::ResizeLeft)
        | (m, Right, Action::ResizeRight)
            if m == shift =>
        {
            ProgramFirst
        }
        (m, Up, Action::JumpPrevPrompt) | (m, Down, Action::JumpNextPrompt)
            if m == ctrl || (macos && m == Mods::SUPER) =>
        {
            WhenKettleIdle
        }
        (m, PageUp, Action::ScrollPageUp)
        | (m, PageDown, Action::ScrollPageDown)
        | (m, Home, Action::SelectToTop)
        | (m, End, Action::SelectToBottom)
            if m == shift =>
        {
            WhenKettleIdle
        }
        (m, Up, Action::ScrollLineUp)
        | (m, Down, Action::ScrollLineDown)
        | (m, PageDown, Action::MoveTabRight)
        | (m, PageUp, Action::MoveTabLeft)
            if m == ctrl_shift =>
        {
            WhenKettleIdle
        }
        (m, Home, Action::ScrollToTop)
        | (m, End, Action::ScrollToBottom)
        | (m, PageDown, Action::NextTab)
        | (m, PageUp, Action::PrevTab)
            if m == ctrl =>
        {
            WhenKettleIdle
        }
        (m, Char('n'), Action::FocusNext) | (m, Char('p'), Action::FocusPrev)
            if m == ctrl_shift =>
        {
            WhenKettleIdleAndDistinct
        }
        // macOS's two directional focus chords. Alt+Arrow elsewhere already
        // falls through when it has nowhere to go
        // (`adaptive_focus_chord_falls_through`).
        (m, Up, Action::FocusUp)
        | (m, Down, Action::FocusDown)
        | (m, Left, Action::FocusLeft)
        | (m, Right, Action::FocusRight)
            if macos && (m == Mods::ALT | Mods::SUPER || m == Mods::CTRL | Mods::SUPER) =>
        {
            WhenKettleIdleAndDistinct
        }
        // Alt+1-9 everywhere, and Cmd+1-9 on macOS.
        (m, Char(digit @ '1'..='9'), Action::GotoTab(index))
            if (m == Mods::ALT || (macos && m == Mods::SUPER))
                && u32::from(*index) + 1 == digit.to_digit(10).unwrap_or(0) =>
        {
            WhenKettleIdle
        }
        _ => return None,
    };
    Some(rule)
}

/// The window state that decides whether a Kettle action would do anything.
struct KettleActionState {
    tabs: usize,
    current_tab: usize,
    /// The pane has a Kettle selection (even an empty one a click left), which
    /// `Shift+Home/End` would extend.
    selection_exists: bool,
    /// A focus action has a visible pane to move to.
    focus_target_exists: bool,
    /// The pane's keyboard mode encodes the chord as itself (see
    /// [`chord_reaches_program_distinctly`]).
    chord_is_distinct: bool,
}

/// Whether Kettle's action would change nothing if it ran now, with the view
/// at the bottom (`program_gets_chord` never asks otherwise).
fn kettle_action_is_idle(
    action: &Action,
    claims: &kettle_core::KeyboardClaims,
    kettle: &KettleActionState,
) -> bool {
    let no_history = claims.alt_screen || claims.history_size == 0;
    match action {
        Action::JumpPrevPrompt => !claims.prompt_above,
        Action::JumpNextPrompt => !claims.prompt_below,
        Action::ScrollToTop | Action::ScrollPageUp | Action::ScrollLineUp => no_history,
        Action::ScrollToBottom | Action::ScrollPageDown | Action::ScrollLineDown => true,
        // With nothing selected, Kettle would start a selection at the
        // program's cursor; a program that owns the keyboard wants the key.
        Action::SelectToTop | Action::SelectToBottom => !kettle.selection_exists,
        Action::NextTab | Action::PrevTab | Action::MoveTabLeft | Action::MoveTabRight => {
            kettle.tabs < 2
        }
        Action::GotoTab(index) => {
            let index = usize::from(*index);
            index >= kettle.tabs || index == kettle.current_tab
        }
        Action::FocusUp
        | Action::FocusDown
        | Action::FocusLeft
        | Action::FocusRight
        | Action::FocusNext
        | Action::FocusPrev => !kettle.focus_target_exists,
        _ => false,
    }
}

/// Whether a program that owns the keyboard gets a chord with this rule.
///
/// A view scrolled into history keeps every `WhenKettleIdle` chord Kettle's:
/// a key that reached the program would snap the view back to the bottom
/// (`scroll-on-keystroke`), so the user reading scrollback would lose their
/// place at its top, and a held key would bounce between the two.
fn program_gets_chord(
    rule: ProgramKeyRule,
    action: &Action,
    claims: &kettle_core::KeyboardClaims,
    kettle: &KettleActionState,
) -> bool {
    if !claims.program_owns_keyboard() {
        return false;
    }
    match rule {
        ProgramKeyRule::ProgramFirst => true,
        ProgramKeyRule::WhenKettleIdle => {
            claims.display_offset == 0 && kettle_action_is_idle(action, claims, kettle)
        }
        ProgramKeyRule::WhenKettleIdleAndDistinct => {
            kettle.chord_is_distinct
                && claims.display_offset == 0
                && kettle_action_is_idle(action, claims, kettle)
        }
    }
}

/// Whether the pane's keyboard mode sends this chord as a key of its own: it
/// encodes to bytes, and those bytes differ from the same key with any one of
/// its modifiers released. Plain legacy mode sends Ctrl+Shift+N exactly as
/// Ctrl+N and a Command chord as nothing, so a program could only mistake the
/// one for another key or never see the other.
fn chord_reaches_program_distinctly(trigger: Trigger, mode: kettle_core::TermMode) -> bool {
    // The press below carries no text. A real Ctrl+Shift+N does, its Control
    // character, and with kitty flags that neither disambiguate nor report all
    // keys (event types alone, say) that text sends it down the legacy path as
    // Ctrl+N. Only those two flags build the kitty sequence for a character
    // key whatever its text, so require one of them.
    if matches!(trigger.key, KKey::Char(_))
        && mode.intersects(kettle_core::TermMode::KITTY_KEYBOARD_PROTOCOL)
        && !mode.intersects(
            kettle_core::TermMode::DISAMBIGUATE_ESC_CODES
                | kettle_core::TermMode::REPORT_ALL_KEYS_AS_ESC,
        )
    {
        return false;
    }
    let Some(key) = chord_key(trigger.key) else {
        return false;
    };
    let mods = chord_modifiers(trigger.mods);
    let Some(bytes) = crate::input::encode_key_press(&key, mods, mode) else {
        return false;
    };
    [
        ModifiersState::SHIFT,
        ModifiersState::CONTROL,
        ModifiersState::ALT,
        ModifiersState::SUPER,
    ]
    .into_iter()
    .filter(|m| mods.contains(*m))
    .all(|m| crate::input::encode_key_press(&key, mods - m, mode).as_deref() != Some(&bytes[..]))
}

/// The winit key for the keys a [`ProgramKeyRule::WhenKettleIdleAndDistinct`]
/// chord uses. A shifted letter is the lowercase character with Shift held, as
/// winit reports it once the modifier is applied by the keybind lookup.
fn chord_key(key: KKey) -> Option<Key> {
    Some(match key {
        KKey::Up => Key::Named(NamedKey::ArrowUp),
        KKey::Down => Key::Named(NamedKey::ArrowDown),
        KKey::Left => Key::Named(NamedKey::ArrowLeft),
        KKey::Right => Key::Named(NamedKey::ArrowRight),
        KKey::Char(c) => Key::Character(c.to_string().into()),
        _ => return None,
    })
}

fn chord_modifiers(m: Mods) -> ModifiersState {
    let mut out = ModifiersState::empty();
    out.set(ModifiersState::SHIFT, m.contains(Mods::SHIFT));
    out.set(ModifiersState::CONTROL, m.contains(Mods::CTRL));
    out.set(ModifiersState::ALT, m.contains(Mods::ALT));
    out.set(ModifiersState::SUPER, m.contains(Mods::SUPER));
    out
}

fn parse_ctl_mods(
    value: Option<&serde_json::Value>,
) -> std::result::Result<ModifiersState, String> {
    let mut mods = ModifiersState::empty();
    let Some(value) = value else {
        return Ok(mods);
    };
    let parts: Vec<String> = if let Some(s) = value.as_str() {
        if s.trim().is_empty() {
            Vec::new()
        } else {
            s.split('+').map(|p| p.trim().to_string()).collect()
        }
    } else if let Some(arr) = value.as_array() {
        let mut out = Vec::with_capacity(arr.len());
        for item in arr {
            let Some(s) = item.as_str() else {
                return Err("'mods' array entries must be strings".to_string());
            };
            out.push(s.to_string());
        }
        out
    } else {
        return Err("'mods' must be a string like 'ctrl+shift' or an array".to_string());
    };
    for part in parts {
        match part.trim().to_ascii_lowercase().as_str() {
            "" => {}
            "ctrl" | "control" => mods |= ModifiersState::CONTROL,
            "alt" | "opt" | "option" => mods |= ModifiersState::ALT,
            "shift" => mods |= ModifiersState::SHIFT,
            "super" | "cmd" | "command" | "win" | "windows" | "meta" | "logo" => {
                mods |= ModifiersState::SUPER;
            }
            other => return Err(format!("unknown modifier '{other}'")),
        }
    }
    Ok(mods)
}

fn parse_ctl_logical_key(
    value: Option<&serde_json::Value>,
) -> std::result::Result<Option<Key>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(token) = value.as_str() else {
        return Err("'logical' must be a key token string".to_string());
    };
    if matches!(
        token.trim().to_ascii_lowercase().as_str(),
        "" | "none" | "unidentified"
    ) {
        return Ok(None);
    }
    let Some((mods, key)) = parse_send_key(token) else {
        return Err(format!("unrecognized logical key token '{token}'"));
    };
    if !mods.is_empty() {
        return Err("'logical' must be one key token without modifiers".to_string());
    }
    Ok(Some(key))
}

fn parse_ctl_physical_key(
    value: Option<&serde_json::Value>,
) -> std::result::Result<Option<PhysicalKey>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    let Some(raw) = value.as_str() else {
        return Err("'physical' must be a key-code string".to_string());
    };
    let normalized: String = raw
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    if matches!(normalized.as_str(), "" | "none" | "unidentified") {
        return Ok(None);
    }
    let code = match normalized.as_str() {
        "equal" | "equals" => KeyCode::Equal,
        "minus" => KeyCode::Minus,
        "digit0" | "key0" | "0" => KeyCode::Digit0,
        "numpadadd" | "numpadplus" => KeyCode::NumpadAdd,
        "numpadsubtract" | "numpadminus" => KeyCode::NumpadSubtract,
        "numpadequal" | "numpadequals" => KeyCode::NumpadEqual,
        "numpad0" => KeyCode::Numpad0,
        _ => return Err(format!("unsupported physical key code '{raw}'")),
    };
    Ok(Some(PhysicalKey::Code(code)))
}

fn to_kkey(key: &Key) -> Option<KKey> {
    Some(match key {
        Key::Character(s) => KKey::Char(s.chars().next()?.to_ascii_lowercase()),
        Key::Named(n) => match n {
            NamedKey::ArrowUp => KKey::Up,
            NamedKey::ArrowDown => KKey::Down,
            NamedKey::ArrowLeft => KKey::Left,
            NamedKey::ArrowRight => KKey::Right,
            NamedKey::PageUp => KKey::PageUp,
            NamedKey::PageDown => KKey::PageDown,
            NamedKey::Home => KKey::Home,
            NamedKey::End => KKey::End,
            NamedKey::Enter => KKey::Enter,
            NamedKey::Tab => KKey::Tab,
            // winit reports the space bar as a named key, never as the
            // character " ", so without this the `ctrl+shift+space` vi-mode
            // default, and any `space` chord in a config, never matched.
            NamedKey::Space => KKey::Char(' '),
            // Without these two the `cmd+backspace` default parses out of the
            // config and then never matches a real key press.
            NamedKey::Backspace => KKey::Backspace,
            NamedKey::Delete => KKey::Delete,
            NamedKey::F1 => KKey::F(1),
            NamedKey::F2 => KKey::F(2),
            NamedKey::F3 => KKey::F(3),
            NamedKey::F4 => KKey::F(4),
            NamedKey::F5 => KKey::F(5),
            NamedKey::F6 => KKey::F(6),
            NamedKey::F7 => KKey::F(7),
            NamedKey::F8 => KKey::F(8),
            NamedKey::F9 => KKey::F(9),
            NamedKey::F10 => KKey::F(10),
            NamedKey::F11 => KKey::F(11),
            NamedKey::F12 => KKey::F(12),
            _ => return None,
        },
        _ => return None,
    })
}

/// Whether a captured `(mods, key)` chord is safe to bind from the settings
/// keybind-capture overlay.
///
/// A modifier-LESS chord is rejected unless the key is an F-key. The global
/// key path matches keybinds before text encoding, so binding a bare `a` (a
/// mis-press during capture) would make every future `a` fire the action
/// instead of typing, across all panes, persisted across restarts, with no
/// in-overlay unbind. Enter/Tab/arrows are likewise essential unmodified, so
/// only F-keys (which produce no text) may be bound without a modifier.
fn keybind_chord_is_safe(mods: Mods, key: KKey) -> bool {
    !mods.is_empty() || matches!(key, KKey::F(_))
}

fn to_mods(m: ModifiersState) -> Mods {
    let mut out = Mods::empty();
    if m.shift_key() {
        out |= Mods::SHIFT;
    }
    if m.control_key() {
        out |= Mods::CTRL;
    }
    if m.alt_key() {
        out |= Mods::ALT;
    }
    if m.super_key() {
        out |= Mods::SUPER;
    }
    out
}

/// A row hint adds dependencies or timing, rather than repeating the control.
/// Whether `saved` would show Kettle in another language than `running`.
/// `os_locale` runs only for `auto`.
fn language_change_pending(
    saved: kettle_config::LanguagePreference,
    running: kettle_i18n::Language,
    os_locale: impl FnOnce() -> Option<String>,
) -> bool {
    saved.resolve(os_locale) != running
}

fn settings_footer_note(
    tr: &kettle_i18n::Translator,
    active_gpu: Option<&str>,
    focused_key: Option<&str>,
    restart_pending: bool,
    language_pending: bool,
    agent_previews: Option<kettle_i18n::Text>,
) -> Option<String> {
    use kettle_i18n::Text;
    let mut notes = Vec::new();
    if let Some(gpu) = active_gpu {
        notes.push(gpu);
    }
    match focused_key {
        Some("agent-display" | "agent-display-claude-code" | "agent-display-codex") => {
            notes.extend(agent_previews.map(|note| tr.text(note)));
        }
        Some("window-blur" | "background-opacity") => {
            notes.push(tr.text(Text::SettingsNoteBlur));
        }
        Some("completion-overlay") => notes.push(tr.text(Text::SettingsNoteNewShells)),
        // A new window keeps the process's language, unlike the changes the
        // general restart note covers.
        Some("language") if !language_pending => {
            notes.push(tr.text(Text::SettingsNoteLanguage));
        }
        _ => {}
    }
    if language_pending {
        notes.push(tr.text(Text::SettingsNoteLanguagePending));
    }
    if restart_pending {
        // The pending flag records no cause, even when Graphics is selected.
        notes.push(tr.text(Text::SettingsNoteRestart));
    }
    (!notes.is_empty()).then(|| notes.join("\n"))
}

/// What the Claude Code row's footer says: why new panes get no plugin, or
/// else when the setting applies.
fn claude_code_note(
    display_on: bool,
    refusal: Option<crate::agent_plugin::PluginRefusal>,
) -> kettle_i18n::Text {
    use crate::agent_plugin::PluginRefusal;
    use kettle_i18n::Text;
    match refusal {
        Some(PluginRefusal::SideloadDisabled) => Text::SettingsNoteClaudeCodeForbidden,
        Some(PluginRefusal::Translocated) => Text::SettingsNoteClaudeCodeTranslocated,
        Some(
            PluginRefusal::UnnamedExecutable
            | PluginRefusal::UnnamedDirectory
            | PluginRefusal::Unverified,
        ) => Text::SettingsNoteClaudeCodeUnavailable,
        Some(PluginRefusal::Unsupported) => Text::SettingsNoteClaudeCodeUnsupported,
        None if !display_on => Text::SettingsNoteClaudeCodeNeedsPreviews,
        None => Text::SettingsNoteClaudeCodeNewPanes,
    }
}

/// What the Codex row's footer says: why new panes' shells get no `codex`,
/// or else where and when the setting applies.
fn codex_note(
    display_on: bool,
    refusal: Option<crate::codex_shell::ShellRefusal>,
) -> kettle_i18n::Text {
    use crate::codex_shell::ShellRefusal;
    use kettle_i18n::Text;
    match refusal {
        Some(ShellRefusal::Translocated) => Text::SettingsNoteCodexTranslocated,
        Some(ShellRefusal::Unavailable) => Text::SettingsNoteCodexUnavailable,
        Some(ShellRefusal::Unsupported) => Text::SettingsNoteCodexUnsupported,
        None if !display_on => Text::SettingsNoteCodexNeedsPreviews,
        None => Text::SettingsNoteCodexNewPanes,
    }
}

/// What the Agent previews row's footer says: the one fact that explains
/// the row's effect right now, most surprising first. `launch_allows` is
/// whether this launch's flags let the row turn display on at all.
fn agent_previews_note(
    saved: bool,
    policy: kettle_ctl::CtlPolicy,
    launch_allows: bool,
    server_running: bool,
    start_attempted: bool,
) -> kettle_i18n::Text {
    use kettle_i18n::Text;
    if policy.runs_server() && !server_running && start_attempted {
        Text::SettingsNoteAgentPreviewsUnavailable
    } else if policy.server() == kettle_config::AgentServer::Full {
        Text::SettingsNoteAgentPreviewsFull
    } else if !launch_allows && !policy.display() {
        Text::SettingsNoteAgentPreviewsLaunchOff
    } else if !saved && policy.display() {
        Text::SettingsNoteAgentPreviewsPendingOff
    } else {
        Text::SettingsNoteAgentPreviews
    }
}

/// The Settings footer: each key followed by what it does. The keys are the
/// same in every language; the vim keys show when `vim-menu-nav` is on.
fn settings_hints(tr: &kettle_i18n::Translator, vim_nav: bool) -> String {
    use kettle_i18n::Text;
    let hints: &[(&str, Text)] = if vim_nav {
        &[
            ("↑↓/jk", Text::SettingsHintField),
            ("←→/hl", Text::SettingsHintChange),
            ("g/G", Text::SettingsHintEnds),
            ("Tab", Text::SettingsHintCategory),
            ("Esc", Text::SettingsHintClose),
        ]
    } else {
        &[
            ("↑↓", Text::SettingsHintField),
            ("←→", Text::SettingsHintChange),
            ("Tab", Text::SettingsHintCategory),
            ("Esc", Text::SettingsHintClose),
        ]
    };
    hints
        .iter()
        .map(|(keys, what)| format!("{keys} {}", tr.text(*what)))
        .collect::<Vec<_>>()
        .join("    ")
}

/// A GPU kind from the renderer's adapter info, in the UI's language. An
/// unknown kind shows as the renderer gave it.
fn gpu_kind_text<'a>(tr: &kettle_i18n::Translator, kind: &'a str) -> &'a str {
    use kettle_i18n::Text;
    match kind {
        "Discrete" => tr.text(Text::SettingsGpuKindDiscrete),
        "Integrated" => tr.text(Text::SettingsGpuKindIntegrated),
        "Virtual" => tr.text(Text::SettingsGpuKindVirtual),
        "Software" => tr.text(Text::SettingsGpuKindSoftware),
        "Other" => tr.text(Text::SettingsGpuKindOther),
        kind => kind,
    }
}

/// How the in-settings text-edit buffer is shown in the value column. A short
/// string shows whole; a long one shows an ellipsized tail, so a long path
/// stays readable (the caret/end is what matters while typing) without
/// ballooning the panel width.
fn settings_edit_display(buf: &str) -> String {
    let count = buf.chars().count();
    if count <= 40 {
        return buf.to_string();
    }
    let tail: String = buf.chars().skip(count - 39).collect();
    format!("…{tail}")
}

/// Startup visibility policy. Windows are created hidden while renderer init
/// runs, then visible states are revealed once the wgpu surface is configured.
/// Only `window_state = hidden` remains hidden.
fn should_reveal_after_renderer_init(state: kettle_config::WindowState) -> bool {
    !matches!(state, kettle_config::WindowState::Hidden)
}

/// Some surface backends cannot acquire a first frame while their native
/// window is hidden. macOS Metal is one: `CAMetalLayer::nextDrawable` does not
/// make progress until the `NSWindow` can be ordered on screen. In that case a
/// hide-until-first-presentation policy is circular, so reveal after renderer
/// initialization and immediately before the first surface frame instead.
fn should_reveal_before_first_surface_frame(
    state: kettle_config::WindowState,
    window_shown: bool,
    surface_requires_visible_window: bool,
) -> bool {
    surface_requires_visible_window && !window_shown && should_reveal_after_renderer_init(state)
}

/// Where a session snapshot should be written, if anywhere.
#[derive(Debug, PartialEq, Eq)]
enum SessionWriteTarget<'a> {
    NamedLayout(&'a str),
    DefaultSession,
    Skip,
}

/// Decide where a snapshot goes.
///
/// The rule worth stating out loud is the first arm: **an empty snapshot never
/// overwrites a named layout.** `close_window` deliberately empties the mux
/// before saving, so the SESSION it writes is the empty one ("this window is
/// finished, do not bring it back"). That intent belongs to session.json. Sent
/// to a named layout, the empty snapshot would erase the workspace, and the
/// next `--layout NAME` would open a single default pane. Terminator, whose
/// layouts this mirrors, only ever writes one from an explicit Add/Refresh;
/// launching a layout never modifies it.
///
/// Extracted so the truth table can be driven directly; the arms below are the
/// whole of the routing decision.
fn session_write_target(
    layout: Option<&str>,
    named_layout_writable: bool,
    restore_mode: bool,
    snapshot_is_empty: bool,
) -> SessionWriteTarget<'_> {
    match layout {
        // Never clear a named workspace by quitting.
        Some(_) if snapshot_is_empty => SessionWriteTarget::Skip,
        Some(name) if named_layout_writable => SessionWriteTarget::NamedLayout(name),
        // `--layout NAME` given, but a launch override replaced it: leave the
        // file alone rather than overwriting it with a window that was never
        // restored from it.
        Some(_) => SessionWriteTarget::Skip,
        // Only write the DEFAULT session.json when this launch is in restore
        // mode — symmetric with the opt-in load gate. A fresh (non-opted-in)
        // window must NOT overwrite the saved layout that `--restore` /
        // `restore-session = true` exists to recover.
        None if restore_mode => SessionWriteTarget::DefaultSession,
        None => SessionWriteTarget::Skip,
    }
}

/// Is the default last-session (`session.json`) active for THIS launch?
/// Drives BOTH the startup restore gate (whether to `load()`) and the
/// `save_session` gate (whether to `save()` the default session). They MUST
/// agree. If *load* is opt-in but *save* is not, a fresh, non-opted-in window
/// silently overwrites the saved layout that `--restore` exists to recover.
/// Routing both through this one predicate keeps them symmetric.
/// `--layout NAME` is independent (its own file, explicit intent) and never
/// consults this gate.
///
/// While another build's primary still runs, typically the old version after
/// an update, the session is its: restoring it here would duplicate its open
/// tabs, and saving would overwrite them (#575). Both wait until it exits.
fn should_restore_session(
    startup_restore: bool,
    cfg_restore_session: bool,
    another_build_running: bool,
) -> bool {
    (startup_restore || cfg_restore_session) && !another_build_running
}

/// Open the remote-command spool for watching. Commands left after a crash
/// must not replay into a new run, so the spool is emptied, unless another
/// build's primary is still running: the spool is shared, and a command it
/// has not claimed yet is still meant for it.
fn initialize_remote_spool(
    path: &std::path::Path,
    another_build_running: bool,
) -> std::io::Result<()> {
    let file = kettle_state::open_private_file(path)?;
    if another_build_running {
        return Ok(());
    }
    file.set_len(0)
}

/// How long to let the synchronous GPU adapter+device
/// init run before treating it as a hung graphics driver. Real init is ~1.5s;
/// this is deliberately generous so a slow-but-working GPU is never killed,
/// while still bounding the worst case (a wedged driver that never returns)
/// to a clean diagnostic exit instead of an infinite invisible-window hang.
const GPU_INIT_TIMEOUT_SECS: u64 = 30;

const GPU_RECOVERY_SETTLE: std::time::Duration = std::time::Duration::from_millis(700);

fn gpu_escalation_label(escalation: kettle_render::AdapterEscalation) -> &'static str {
    match escalation {
        kettle_render::AdapterEscalation::Preferred => "preferred",
        kettle_render::AdapterEscalation::AlternateBackend => "alternate_backend",
        kettle_render::AdapterEscalation::SurfacePreferred => "surface_preferred",
        kettle_render::AdapterEscalation::AnyHardware => "any_hardware",
        kettle_render::AdapterEscalation::ForceSoftware => "force_software",
    }
}

fn gpu_recovery_backoff(attempt: u32) -> std::time::Duration {
    let ms = match attempt {
        0 | 1 => 500,
        2 => 1000,
        3 => 2000,
        _ => 4000,
    };
    std::time::Duration::from_millis(ms)
}

/// How often the process scan may run. It feeds labels and new-pane
/// directories for shells that never send OSC 7.
const REMOTE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

/// `None` to poll now, or when the throttled poll must run instead. Dropping
/// it would leave a `cd` that lands inside the window unseen until a redraw.
fn remote_poll_deferred_until(
    last: Option<std::time::Instant>,
    now: std::time::Instant,
) -> Option<std::time::Instant> {
    let due = last? + REMOTE_POLL_INTERVAL;
    (now < due).then_some(due)
}

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

/// Which pane loses and which gains the focus report when the pane holding
/// the keyboard changes from `reported` to `focused` (`None`: no pane, as
/// while the window lacks OS focus). No change, no reports.
fn focus_report_transition(
    reported: Option<u64>,
    focused: Option<u64>,
) -> (Option<u64>, Option<u64>) {
    if reported == focused {
        (None, None)
    } else {
        (reported, focused)
    }
}

fn animation_wait(
    animation_active: bool,
    background_ms: Option<u64>,
    blink_wait: Option<std::time::Duration>,
) -> Option<std::time::Duration> {
    [
        animation_active.then(|| std::time::Duration::from_millis(33)),
        background_ms.map(|ms| std::time::Duration::from_millis(ms.clamp(16, 1000))),
        blink_wait,
    ]
    .into_iter()
    .flatten()
    .min()
}

fn cursor_blink_active(configured: bool, pane_requests_blink: bool, window_focused: bool) -> bool {
    configured && pane_requests_blink && window_focused
}

fn normalized_ime_preedit(
    text: String,
    selection: Option<(usize, usize)>,
) -> Option<(String, Option<(usize, usize)>)> {
    (!text.is_empty()).then_some((text, selection))
}

fn current_ime_owner(ws: &WindowState) -> crate::window_state::ImePreeditOwner {
    if ws.search.open {
        crate::window_state::ImePreeditOwner::Search
    } else if modal_holds_cursor_steady(ws) {
        crate::window_state::ImePreeditOwner::Other
    } else {
        ws.mux
            .active_focus()
            .map(crate::window_state::ImePreeditOwner::Terminal)
            .unwrap_or(crate::window_state::ImePreeditOwner::Other)
    }
}

fn update_ime_preedit(
    ws: &mut WindowState,
    text: String,
    selection: Option<(usize, usize)>,
) -> bool {
    use crate::window_state::ImePreeditSession;
    let owner = current_ime_owner(ws);
    let generation = ws.ime_focus_generation;
    if let Some(session) = ws.ime_preedit_owner.as_mut()
        && matches!(
            session.owner,
            crate::window_state::ImePreeditOwner::Terminal(_)
        )
        && (session.owner != owner || session.generation != generation)
    {
        if text.is_empty() && !session.empty_after_focus {
            session.empty_after_focus = true;
            ws.ime_preedit = None;
            return true;
        }
        if !session.empty_after_focus {
            ws.ime_preedit = None;
            return true;
        }
        // The first empty Preedit may lead into the old Commit. A later
        // Preedit starts fresh input, including Wayland's empty one before
        // a commit-only input, so the cancelled owner no longer guards it.
        ws.ime_preedit_owner = None;
    }
    let next = normalized_ime_preedit(text, selection);
    if ws.ime_preedit == next {
        return false;
    }
    match (next.is_some(), ws.ime_preedit_owner) {
        // A composition begins here, unless one under way here goes on.
        (true, session)
            if session.is_none_or(|session| {
                session.ended.is_some()
                    || session.owner != owner
                    || session.generation != generation
            }) =>
        {
            ws.ime_preedit_owner = Some(ImePreeditSession {
                owner,
                generation,
                ended: None,
                empty_after_focus: false,
            });
        }
        // Emptied where it began: cancelled, or leading into a Commit for
        // this same surface, so there is nothing to guard.
        (false, Some(session))
            if session.ended.is_none()
                && session.owner == owner
                && session.generation == generation =>
        {
            ws.ime_preedit_owner = None;
        }
        // Emptied after focus moved: a Commit for the old surface may follow.
        (false, Some(session)) if session.ended.is_none() => {
            ws.ime_preedit_owner = Some(ImePreeditSession {
                ended: Some(generation),
                ..session
            });
        }
        _ => {}
    }
    ws.ime_preedit = next;
    true
}

fn take_ime_commit(ws: &mut WindowState) -> bool {
    ws.ime_preedit = None;
    let owner = current_ime_owner(ws);
    let generation = ws.ime_focus_generation;
    ws.ime_preedit_owner.take().is_none_or(|session| {
        // A modal session that ended before focus moved on again is stale.
        // Terminal sessions keep rejecting their old composition.
        (!matches!(
            session.owner,
            crate::window_state::ImePreeditOwner::Terminal(_)
        ) && session.ended.is_some_and(|ended| ended != generation))
            || (session.owner == owner && session.generation == generation)
    })
}

fn ime_preedit_focus(text: &str, selection: Option<(usize, usize)>) -> usize {
    use unicode_segmentation::UnicodeSegmentation as _;

    let requested = selection
        .map_or(text.len(), |(_, focus)| focus)
        .min(text.len());
    let mut boundary = requested;
    while boundary > 0 && !text.is_char_boundary(boundary) {
        boundary -= 1;
    }
    if boundary == text.len() {
        return boundary;
    }
    text.grapheme_indices(true)
        .map(|(index, _)| index)
        .take_while(|index| *index <= boundary)
        .last()
        .unwrap_or(0)
}

/// Watchdog body for the GPU-init guard. Polls `done` every `step`
/// until `timeout` elapses. Returns `true` if the timeout was reached without
/// `done` ever being observed set — i.e. the caller should treat the init as
/// hung — and `false` if `done` was seen in time (init finished, watchdog
/// stands down). Pulled out as a pure-ish helper so the timeout logic is
/// unit-testable without standing up a GPU or calling `process::exit`.
fn gpu_init_timed_out(
    done: &std::sync::atomic::AtomicBool,
    timeout: std::time::Duration,
    step: std::time::Duration,
) -> bool {
    use std::sync::atomic::Ordering;
    let mut waited = std::time::Duration::ZERO;
    while waited < timeout {
        if done.load(Ordering::Acquire) {
            return false;
        }
        std::thread::sleep(step);
        waited += step;
    }
    !done.load(Ordering::Acquire)
}

/// Whether terminal-owned transient chrome may be projected. Keeping this
/// predicate shared prevents a hidden receipt from retaining a hover pause
/// behind a modal surface.
fn terminal_surface_available(ws: &WindowState) -> bool {
    ws.context_menu.is_none()
        && ws.vi_mode.is_none()
        && ws.hint_state.is_none()
        && ws.palette_input.is_none()
        && ws.settings_nav.is_none()
        && ws.layout_picker_input.is_none()
        && ws.theme_picker_input.is_none()
        && ws.ssh_input.is_none()
        && ws.confirm_dialog.is_none()
        && ws.editing_title.is_none()
        && !ws.search.open
        && ws.preview_overlay.is_none()
}

/// Where a pointer event goes while the search bar is open.
///
/// The bar is a reserved lane below the grid, not an overlay: the grid above
/// it is fully visible and stays mouse-interactive, exactly as Terminator
/// keeps its VTE selectable under its search bar. A press inside the bar's
/// rectangle belongs to the bar's controls; a press anywhere else is ordinary
/// grid input (selection, links, scrollbar, mouse reporting). Motion and
/// release follow whichever gesture is live: an editor drag started in the
/// bar keeps the bar, anything else keeps the grid. Keyboard focus never
/// leaves the search editor either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchPointerRoute {
    /// The bar is closed; nothing here applies.
    Closed,
    /// The bar's controls own the event.
    Bar,
    /// The grid owns the event as if the bar were closed.
    Grid,
}

// Captured grid drags keep their motion and release when crossing the bar.
fn search_bar_blocks_mouse_report(in_bar: bool, pressed: bool, motion: bool, held: bool) -> bool {
    in_bar && pressed && (!motion || !held)
}

fn search_pointer_route(
    search_open: bool,
    press_in_bar: Option<bool>,
    dragging_editor: bool,
) -> SearchPointerRoute {
    if !search_open {
        return SearchPointerRoute::Closed;
    }
    match press_in_bar {
        Some(true) => SearchPointerRoute::Bar,
        Some(false) => SearchPointerRoute::Grid,
        None if dragging_editor => SearchPointerRoute::Bar,
        None => SearchPointerRoute::Grid,
    }
}

/// Should an open modal swallow a pointer event (mouse press / wheel)
/// instead of letting it fall through to the tab bar, pane focus, or
/// mouse-tracking *behind* the dialog? Otherwise a click could switch tabs or
/// focus a pane, and a wheel could zoom the font or scroll the pane, while a
/// dialog that seems to capture input sits on top. True whenever
/// `any_modal_open` is set, *except* for a lone context menu, which owns its
/// own click/scroll paths and is re-opened (relocated) by a right-click, so
/// gating it here would break that.
fn modal_swallows_pointer(any_modal_open: bool, context_menu_open: bool) -> bool {
    any_modal_open && !context_menu_open
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
            #[cfg(target_os = "linux")]
            UserEvent::Wayland => self.poll_wayland(el),
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

/// The window waiting for this pane's video preview generation: the window
/// that requested it, or the one its tab has moved to since.
fn video_preview_target_window(
    windows: &std::collections::BTreeMap<u64, WindowState>,
    hint: u64,
    pane: u64,
    generation: u64,
) -> Option<u64> {
    if windows
        .get(&hint)
        .is_some_and(|window| window.expects_video_preview(pane, generation))
    {
        return Some(hint);
    }
    windows.iter().find_map(|(&seq, window)| {
        window
            .expects_video_preview(pane, generation)
            .then_some(seq)
    })
}

/// How a new in-process window starts life.
enum WindowOpen {
    /// A fresh window with one shell tab (`Action::NewWindow`).
    Fresh { cwd: Option<String> },
    /// Adopt a tab detached from another window — the live tab move /
    /// tear-off. PTYs keep running; nothing respawns.
    AdoptTab {
        tab: crate::mux::DetachedTab,
        source_color: Option<kettle_config::Rgb>,
        /// The source window's runtime font size, so the torn-off window
        /// draws its tab and status bars at the size the drag anchor was
        /// measured with.
        font_size: Option<f32>,
        /// The moved tab's paste receipt and pending video preview.
        paste: Box<crate::window_state::MovedPasteState>,
    },
    /// Respawn one saved window of a multi-window session (tabs + geometry)
    /// on `--restore` / `restore-session = true` startup.
    Restore(crate::session::SWindow),
}

/// Is this window's backend Wayland? A runtime check, since X11 vs Wayland is
/// a runtime choice on Linux and `cfg!` can't answer it. Wayland gets the
/// tear-at-release fallback (no client-side window positioning, and
/// `xdg_toplevel.move` is compositor-validated against a press serial a
/// just-created surface never saw).
fn window_is_wayland(w: &winit::window::Window) -> bool {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    matches!(
        w.window_handle().map(|h| h.as_raw()),
        Ok(RawWindowHandle::Wayland(_))
    )
}

/// Tear-off UX, Windows: the LIVE screen cursor. The tear
/// positions the torn window from the CursorMoved event's coordinates,
/// but window creation takes ~50-150ms — by the time `drag_window()`'s
/// posted WM_NCLBUTTONDOWN starts the modal move loop, a fast-moving
/// pointer has slid well past the event position, and DefWindowProc
/// anchors the loop at the CURRENT cursor. Without re-anchoring, the
/// pointer ends up holding the window mid-body instead of at the grab
/// point (measured: ~97px of slide under a scripted drag). Re-positioning
/// from the live cursor immediately before the handoff closes the gap to
/// sub-millisecond.
#[cfg(target_os = "windows")]
fn cursor_screen_pos() -> Option<(f64, f64)> {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    let mut p = POINT::default();
    unsafe { GetCursorPos(&mut p) }
        .ok()
        .map(|()| (f64::from(p.x), f64::from(p.y)))
}

/// Tear-off UX, X11: one `QueryPointer` round-trip on a lazy
/// side connection — the LIVE screen cursor (root coords ARE screen
/// coords) plus the primary button's held state from the same reply's
/// button mask. Needed because the WM's move-grab anchor drifts from the
/// tear-time `grab` offset (measured 55-86px under Mutter — enough to miss
/// the whole dock band), pointer motion stops reaching the client entirely
/// while the WM holds the move grab, and the Esc-cancel tell needs the
/// physical button state (see `primary_button_physically_held`). A Wayland
/// session (no X server for this client) fails the one-time connect and
/// yields `None` forever — callers keep their approximations. Only ever
/// exercised while torn-drag tracking is live.
#[cfg(all(unix, not(target_os = "macos")))]
fn x11_pointer_state() -> Option<((f64, f64), bool)> {
    use x11rb::connection::Connection as _;
    use x11rb::protocol::xproto::{ConnectionExt as _, KeyButMask};
    use x11rb::rust_connection::RustConnection;
    static X11: std::sync::OnceLock<Option<(RustConnection, u32)>> = std::sync::OnceLock::new();
    let (conn, root) = X11
        .get_or_init(|| {
            x11rb::connect(None).ok().map(|(conn, screen)| {
                let root = conn.setup().roots[screen].root;
                (conn, root)
            })
        })
        .as_ref()?;
    let reply = conn.query_pointer(*root).ok()?.reply().ok()?;
    // The server applies the pointer-button mapping before reporting the
    // state mask, so BUTTON1 here is the LOGICAL primary — a left-handed
    // mapping is already accounted for.
    let held = reply.mask.contains(KeyButMask::BUTTON1);
    Some(((f64::from(reply.root_x), f64::from(reply.root_y)), held))
}

/// Tear-off UX, X11: the LIVE screen cursor, counterpart of the Windows
/// `GetCursorPos` path above. See `x11_pointer_state`.
#[cfg(all(unix, not(target_os = "macos")))]
fn cursor_screen_pos() -> Option<(f64, f64)> {
    x11_pointer_state().map(|(pos, _)| pos)
}

#[cfg(target_os = "macos")]
fn cursor_screen_pos() -> Option<(f64, f64)> {
    crate::native_drag::cursor_position()
}

#[cfg(not(any(target_os = "windows", unix)))]
fn cursor_screen_pos() -> Option<(f64, f64)> {
    None
}

// AppKit's global desktop uses points; winit's physical positions multiply
// the entire desktop origin by each window's scale. Normalize before comparing
// windows so a Retina donor can dock onto a non-Retina target.
fn drag_coordinate_scale(window: &Window) -> f64 {
    if cfg!(target_os = "macos") {
        window.scale_factor()
    } else {
        1.0
    }
}

fn drag_position(window: &Window, position: winit::dpi::PhysicalPosition<i32>) -> (f64, f64) {
    let scale = drag_coordinate_scale(window);
    (f64::from(position.x) / scale, f64::from(position.y) / scale)
}

fn drag_native_position(position: (f64, f64)) -> winit::dpi::Position {
    if cfg!(target_os = "macos") {
        winit::dpi::LogicalPosition::new(position.0, position.1).into()
    } else {
        winit::dpi::PhysicalPosition::new(position.0 as i32, position.1 as i32).into()
    }
}

/// Re-dock, Windows: per-window alpha for the dock-hover
/// preview. The torn window rides UNDER the pointer, so at the moment a
/// dock target latches, the full-size torn window is covering the very
/// strip the insertion marker draws in — the user would never see it.
/// Going translucent while a target is latched shows the target strip
/// (and marker) through the dragged window, Chromium-style. Verified
/// live against kettle's wgpu flip-model swapchain (renders fine under
/// WS_EX_LAYERED + LWA_ALPHA). 255 restores and drops the layered bit.
#[cfg(target_os = "windows")]
fn set_window_alpha(hwnd: isize, alpha: u8) {
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::UI::WindowsAndMessaging::{
        GWL_EXSTYLE, GetWindowLongPtrW, LWA_ALPHA, SetLayeredWindowAttributes, SetWindowLongPtrW,
    };
    let h = HWND(hwnd as *mut core::ffi::c_void);
    unsafe {
        let ex = GetWindowLongPtrW(h, GWL_EXSTYLE);
        if alpha == 255 {
            let _ = SetLayeredWindowAttributes(h, COLORREF(0), 255, LWA_ALPHA);
            SetWindowLongPtrW(h, GWL_EXSTYLE, ex & !(WS_EX_LAYERED_BIT));
        } else {
            SetWindowLongPtrW(h, GWL_EXSTYLE, ex | WS_EX_LAYERED_BIT);
            let _ = SetLayeredWindowAttributes(h, COLORREF(0), alpha, LWA_ALPHA);
        }
    }
}

/// Paint a freshly created window's background in the configured terminal
/// background colour, and report whether it took.
///
/// Sets the window class brush that Win32 erases with on `WM_ERASEBKGND`, so
/// the pre-paint window would match what the renderer is about to draw. This
/// is best effort only. A light-background probe showed the pre-paint window
/// stays black whatever the brush is, so the early reveal does not depend on
/// it (see `background_is_dark_enough_to_reveal_early`).
///
/// Returns false if the brush cannot be created.
#[cfg(target_os = "windows")]
fn set_window_background_brush(hwnd: isize, rgb: kettle_config::Rgb) -> bool {
    use windows::Win32::Foundation::{COLORREF, HWND};
    use windows::Win32::Graphics::Gdi::{CreateSolidBrush, DeleteObject, HBRUSH, HGDIOBJ};
    use windows::Win32::UI::WindowsAndMessaging::{GCLP_HBRBACKGROUND, SetClassLongPtrW};

    let kettle_config::Rgb { r, g, b } = rgb;
    // COLORREF is 0x00BBGGRR, not RGB order.
    let colorref = COLORREF(u32::from(r) | (u32::from(g) << 8) | (u32::from(b) << 16));
    let h = HWND(hwnd as *mut core::ffi::c_void);
    unsafe {
        let brush = CreateSolidBrush(colorref);
        if brush.is_invalid() {
            return false;
        }
        // Replaces the class brush and hands back the previous one. The class is
        // shared and outlives the window, and every kettle window wants the same
        // colour. winit registers it with no brush, so a nonzero previous brush
        // came from an earlier kettle window and is destroyed below.
        let previous = SetClassLongPtrW(h, GCLP_HBRBACKGROUND, brush.0 as isize);
        if previous != 0 {
            let _ = DeleteObject(HGDIOBJ(previous as *mut core::ffi::c_void));
        }
        let _ = HBRUSH(brush.0);
        true
    }
}

/// Non-Windows platforms have no equivalent cheap guarantee that the first
/// composited frame is the right colour, so they keep hiding until painted.
#[cfg(not(target_os = "windows"))]
fn set_window_background_brush(_hwnd: isize, _rgb: kettle_config::Rgb) -> bool {
    false
}

/// Current process working set in MiB, for startup phase accounting.
///
/// Kettle's resident footprint is large and barely moves with scrollback, font
/// or features, so it is a FIXED cost. Sampling across the phases that allocate
/// is the only way to attribute it (docs/PERFORMANCE.md has the numbers).
///
/// `0.0` where unavailable; this is diagnostics, never control flow.
#[cfg(target_os = "windows")]
fn process_working_set_mb() -> f64 {
    use windows::Win32::System::ProcessStatus::{GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS};
    use windows::Win32::System::Threading::GetCurrentProcess;
    let Ok(size) = u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>()) else {
        // A zero size would be accepted by the signature and rejected -- or
        // worse, partially honoured -- by the API. Refuse instead of asking for
        // a struct of no bytes.
        return 0.0;
    };
    let mut counters = PROCESS_MEMORY_COUNTERS {
        // `cb` is an in-parameter: the API uses it to decide how much of the
        // struct it may write. `Default` leaves it 0, which is not a smaller
        // request, it is a malformed one.
        cb: size,
        ..Default::default()
    };
    unsafe {
        if GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size).is_ok() {
            return counters.WorkingSetSize as f64 / (1024.0 * 1024.0);
        }
    }
    0.0
}

#[cfg(not(target_os = "windows"))]
fn process_working_set_mb() -> f64 {
    0.0
}

/// May the window be shown BEFORE the first painted frame?
///
/// Only when the terminal's background is dark enough that the difference from
/// black is imperceptible, because black is what an unpainted window actually
/// shows. The class brush set alongside this does NOT decide what appears. With
/// a light background configured (`#f0e8d8`) the pre-paint window still sampled
/// `0,0,0`, because winit owns `WM_ERASEBKGND`. Revealing a light-themed window
/// early would trade a late window for a black flash.
///
/// Dark backgrounds have no such trade. `#101010` is 16 levels from black on
/// each channel, which nobody can see, and revealing early shows the window
/// during the ~730ms of renderer init instead of after it.
///
/// The threshold is deliberately strict. This is not "dark theme" detection; it
/// is "indistinguishable from black", so anything with visible colour keeps
/// hiding until painted.
fn background_is_dark_enough_to_reveal_early(bg: kettle_config::Rgb) -> bool {
    // Windows only. "An unpainted window is black" was observed on DWM and is
    // NOT a portable fact; on another compositor the pre-paint frame could as
    // easily be white or transparent.
    if !cfg!(target_os = "windows") {
        return false;
    }
    const MAX_CHANNEL: u8 = 24;
    bg.r <= MAX_CHANNEL && bg.g <= MAX_CHANNEL && bg.b <= MAX_CHANNEL
}

/// `WS_EX_LAYERED` as the isize `GetWindowLongPtrW` works in.
#[cfg(target_os = "windows")]
const WS_EX_LAYERED_BIT: isize = 0x0008_0000;

/// Torn-window alpha while a dock target is latched. Translucent enough to
/// read the target strip + insertion marker through the dragged window,
/// opaque enough that the window still reads as "the thing you're holding"
/// (probed live at several values).
#[cfg(target_os = "windows")]
const DOCK_HOVER_ALPHA: u8 = 150;

/// Is the primary mouse button PHYSICALLY held right now? Distinguishes an
/// Esc-CANCELLED move loop (button still down when the WM ends its grab)
/// from a real drop (button up); the client-visible events look identical
/// for both. Windows honors a swapped-button mouse (`SM_SWAPBUTTON`:
/// GetAsyncKeyState reports PHYSICAL buttons, so the primary button of a
/// left-handed mouse is VK_RBUTTON); X11 reads the button mask from the
/// same `QueryPointer` the live-cursor path uses (the server pre-applies
/// the pointer mapping there). macOS uses the combined session button state.
/// False elsewhere (Wayland never produces a release-while-held; an X11 query failure also
/// degrades to false, which errs toward committing a real drop rather
/// than dropping user data on a cancel we cannot prove).
fn primary_button_physically_held() -> bool {
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        x11_pointer_state().is_some_and(|(_, held)| held)
    }
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            GetAsyncKeyState, VK_LBUTTON, VK_RBUTTON,
        };
        use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_SWAPBUTTON};
        let vk = if unsafe { GetSystemMetrics(SM_SWAPBUTTON) } != 0 {
            VK_RBUTTON
        } else {
            VK_LBUTTON
        };
        (unsafe { GetAsyncKeyState(i32::from(vk.0)) } as u16) & 0x8000 != 0
    }
    #[cfg(target_os = "macos")]
    {
        crate::native_drag::primary_held()
    }
    #[cfg(not(any(target_os = "windows", unix)))]
    false
}

/// HWND of a winit window.
/// `None` off-Windows (the `Win32` raw-handle variant exists on every
/// platform, so this compiles everywhere without cfg noise).
fn window_hwnd(w: &winit::window::Window) -> Option<isize> {
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    match w.window_handle().ok().map(|h| h.as_raw()) {
        Some(RawWindowHandle::Win32(h)) => Some(h.hwnd.get()),
        _ => None,
    }
}

/// Resolve overlapping dock candidates, and foreign windows covering a band,
/// against the REAL z-order. Walks down from the torn window (topmost while
/// dragged); the first visible, uncloaked window containing the cursor
/// decides: one of ours → that's the target; a foreign window → it covers the
/// band, no dock. The cloak check matters: suspended UWP apps park full-screen
/// DWM-cloaked windows in the z-order that would otherwise always read as
/// "covered".
///
/// Known limitation (accepted): always-on-top foreign windows
/// sit ABOVE the torn window and are invisible to a downward walk — a
/// band covered by one still shows the marker (the merge itself is
/// unaffected). GetWindowRect also includes the invisible Win10/11 resize
/// borders, which mirrors real input routing (those borders DO capture
/// clicks), so near-edge "covered" verdicts match what a click would do.
#[cfg(target_os = "windows")]
fn zorder_pick(
    torn_hwnd: isize,
    cursor: (f64, f64),
    candidates: &[(isize, u64, usize)],
) -> Option<(u64, usize)> {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::Graphics::Dwm::{DWMWA_CLOAKED, DwmGetWindowAttribute};
    use windows::Win32::UI::WindowsAndMessaging::{
        GW_HWNDNEXT, GetWindow, GetWindowRect, IsWindowVisible,
    };
    let (px, py) = (cursor.0 as i32, cursor.1 as i32);
    let mut hwnd = unsafe { GetWindow(HWND(torn_hwnd as *mut core::ffi::c_void), GW_HWNDNEXT) };
    // Bounded walk: the desktop can hold hundreds of top-level windows but
    // a runaway loop must be impossible.
    for _ in 0..1024 {
        let Ok(h) = hwnd else { break };
        if unsafe { IsWindowVisible(h) }.as_bool() {
            let mut cloaked: u32 = 0;
            let _ = unsafe {
                DwmGetWindowAttribute(
                    h,
                    DWMWA_CLOAKED,
                    (&raw mut cloaked).cast(),
                    std::mem::size_of::<u32>() as u32,
                )
            };
            let mut r = RECT::default();
            if cloaked == 0
                && unsafe { GetWindowRect(h, &mut r) }.is_ok()
                && px >= r.left
                && px < r.right
                && py >= r.top
                && py < r.bottom
            {
                let found = h.0 as isize;
                return candidates
                    .iter()
                    .find(|(ch, _, _)| *ch == found)
                    .map(|&(_, s, i)| (s, i));
            }
        }
        hwnd = unsafe { GetWindow(h, GW_HWNDNEXT) };
    }
    None
}

/// The live monitor rects, for clamping a saved window geometry whose
/// monitor is gone (see `session::clamp_geometry_to_monitors`).
fn monitor_rects(event_loop: &ActiveEventLoop) -> Vec<(i32, i32, u32, u32)> {
    event_loop
        .available_monitors()
        .map(|m| {
            let p = m.position();
            let s = m.size();
            (p.x, p.y, s.width, s.height)
        })
        .collect()
}

impl App {
    /// Does any pane in this window have PTY output newer than the
    /// window's last paint? (`Terminal::output_generation` vs the snapshot
    /// `redraw` records.) Plain output emits no TermEvent, so this counter
    /// is the only reliable per-window "new output" signal for the fan-out
    /// wakeup.
    fn window_has_new_output(&self, ws: &WindowState) -> bool {
        ws.mux.panes.iter().any(|(id, p)| {
            ws.seen_output_gen.get(id).copied().unwrap_or(0) != p.term.output_generation()
        })
    }

    fn sync_output_wake_gate(&self, ws: &WindowState) {
        let render_quiesced = output_wakeup_must_quiesce(
            window_is_render_hidden(ws),
            ws.frame_recovery.has_pending(),
            ws.renderer.is_none() || self.gpu.as_ref().is_some_and(|gpu| gpu.is_lost()),
        );
        for pane in ws.mux.panes.values() {
            // A hidden surface does not need paint wakeups, but an opt-in
            // recorder/Lua sidechannel still needs event-loop service. Its
            // bounded lossless queue otherwise fills after eight PTY chunks
            // and blocks parsing indefinitely while the window is occluded.
            pane.term
                .set_output_wake_enabled(output_wake_transport_enabled(
                    render_quiesced,
                    pane.output_rx.is_some(),
                ));
        }
    }

    fn acknowledge_output_wakes(&self, ws: &WindowState) {
        for pane in ws.mux.panes.values() {
            pane.term.acknowledge_output_wake();
        }
    }

    fn resumed_inner(&mut self, ws: &mut WindowState, event_loop: &ActiveEventLoop) {
        if ws.window.is_some() {
            return;
        }
        crate::startup_trace::mark(crate::startup_trace::Phase::Resumed);
        // Explicit command/cwd launches never restore a saved workspace.
        // Preserve the fields until the existing consumed-once spawn path
        // below, but decide and preflight the restore before native resources
        // are allocated.
        let has_launch_override = self.launch_override;
        // A launch override means the named layout is not loaded, so it must
        // not be written back either — otherwise the flag that names a layout
        // is the same flag that destroys it.
        self.named_layout_writable = !has_launch_override;
        let loaded_session = if ws.pre_launch.is_some() || has_launch_override {
            None
        } else {
            self.load_startup_session()
        };
        let live_monitors = monitor_rects(event_loop);
        // The size a saved window without geometry gets, in physical pixels
        // like the monitor rects it is validated against: the same startup
        // rule a fresh window uses, so a restore and a first launch agree.
        let monitor = startup_monitor(event_loop);
        if let Some(pre_launch) = ws.pre_launch {
            let matched = monitor == Some(pre_launch);
            crate::startup_trace::note_monitor_match(matched);
            if !matched {
                log::warn!("startup display changed after the pre-launch spawn");
            }
        }
        let default_surface = {
            let (w, h) = startup_inner_size(&self.cfg, monitor);
            let scale = monitor.map_or(1.0, |m| m.scale);
            (
                (w * scale).round().max(1.0) as u32,
                (h * scale).round().max(1.0) as u32,
            )
        };
        let restore_plan = loaded_session.as_ref().and_then(|session| {
            if session.is_empty() {
                return None;
            }
            let windows = match session.validated_restore_windows() {
                Ok(windows) => windows,
                Err(error) => {
                    self.stash_unrestorable_session(&error);
                    return None;
                }
            };
            let geometries = match crate::session::validated_restore_surface_geometries(
                &windows,
                &live_monitors,
                default_surface,
            ) {
                Ok(geometries) => geometries,
                Err(error) => {
                    self.stash_unrestorable_session(&error);
                    return None;
                }
            };
            Some((windows, geometries))
        });

        // Measure the fonts before the window. A pane's grid needs only the
        // cell, so a fresh window can size itself to exactly the configured
        // grid and start its first pane while the window and GPU initialize.
        // The system fonts have been loading on their own thread since
        // `run_with` began, and only the measurement needs this scale. The
        // renderer takes the fonts over.
        let startup_scale = monitor.map_or(1.0, |m| m.scale) as f32;
        if ws.pre_launch.is_none() {
            self.prepare_startup_fonts(ws, startup_scale);
        }
        // A restored window keeps its saved geometry, and a maximized or
        // full-screen one gets its size from the OS after creation. Only a
        // fresh normal or hidden window knows its size before it exists.
        let early_start = restore_plan.is_none()
            && matches!(
                self.cfg.window_state,
                kettle_config::WindowState::Normal | kettle_config::WindowState::Hidden
            );
        let startup_surface = if ws.pre_launch.is_some() {
            ws.startup_surface
        } else if early_start {
            self.startup_surface(ws, monitor)
        } else {
            None
        };
        ws.startup_surface = startup_surface;
        // The pane starts before the window only when the window will open at
        // this scale and size: on the monitor the cell was measured for, which
        // a configured position could change, and within it, where the OS
        // would shrink it. Otherwise it starts after the renderer, at the
        // window's real size.
        let spawned_early = ws.pre_launch.is_some()
            || startup_surface.is_some_and(|size| fits_monitor(size, monitor))
                && self.cfg.window_position_x.is_none()
                && self.cfg.window_position_y.is_none();
        if ws.pre_launch.is_none()
            && spawned_early
            && let Err(e) = self.spawn_first_tab(ws, has_launch_override)
        {
            log::error!("{e}");
            event_loop.exit();
            return;
        }
        if spawned_early && ws.pre_launch.is_none() {
            crate::startup_trace::mark(crate::startup_trace::Phase::PaneSpawned);
            crate::startup_trace::note_path(crate::startup_trace::StartupPath::ResumedEarly);
        }

        // Create the window hidden while renderer init runs on the event-loop
        // thread. The first successful paint reveals it; `window_state =
        // hidden` remains hidden.
        ws.window_shown = !should_reveal_after_renderer_init(self.cfg.window_state);
        ws.startup_window = true;
        let mut attrs = self.window_attributes(self.cfg.window_state, monitor);
        if let Some((width, height)) = startup_surface {
            attrs = attrs.with_inner_size(winit::dpi::PhysicalSize::new(width, height));
        }
        if let Some(surface) = restore_plan
            .as_ref()
            .and_then(|(_, surfaces)| surfaces.first())
        {
            let (width, height) = surface.size;
            attrs = attrs.with_inner_size(winit::dpi::PhysicalSize::new(width, height));
            if let Some((x, y)) = surface.position {
                attrs = attrs.with_position(winit::dpi::PhysicalPosition::new(x, y));
            }
        }
        // Startup is measured, not guessed: the window is created HIDDEN and
        // revealed at the first paint, so everything between here and that
        // reveal is time the user spends looking at nothing. A comparator
        // benchmark can only report the total.
        let t_startup = std::time::Instant::now();
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                log::error!("failed to create window: {e}");
                ws.mux.kill_children();
                event_loop.exit();
                return;
            }
        };
        self.apply_post_create(&window);
        ws.native_fullscreen = Some(crate::native_fullscreen::NativeFullscreen::install(
            &window,
            ws.seq,
            self.proxy.clone(),
        ));
        ws.native_caption_drag = Some(crate::native_drag::NativeCaptionDrag::install(&window));
        ws.native_material = Some(crate::native_material::NativeMaterial::install(
            &window, &self.cfg,
        ));
        crate::startup_trace::mark(crate::startup_trace::Phase::WindowCreated);
        // Measured AFTER apply_post_create because the gpu figure below is
        // derived by subtraction; stopping the clock earlier would charge that
        // setup to the GPU.
        let create_window_ms = t_startup.elapsed().as_secs_f64() * 1000.0;
        let t_a11y = std::time::Instant::now();
        let accessibility = Self::new_accessibility_adapter(
            event_loop,
            &window,
            ws.seq,
            self.proxy.clone(),
            &self.ui_text,
        );
        let a11y_ms = t_a11y.elapsed().as_secs_f64() * 1000.0;

        // Reveal AFTER the accessibility adapter. AccessKit's winit adapter
        // panics if the window has already been shown when it is constructed
        // ("must be created before the window is shown (made visible) for the
        // first time"). The reveal has to sit between the adapter and renderer
        // init, and this ordering is load-bearing.
        //
        // Reveal before the GPU is ready only when the background is close
        // enough to black (see `background_is_dark_enough_to_reveal_early`).
        // Renderer init is ~730ms of startup, and on Windows an unpainted
        // window is black, so a near-black window can appear that much sooner
        // with no visible flash. Otherwise `window_shown` stays false and the
        // first paint reveals the window.
        //
        // The class brush is set to the terminal background as a best effort
        // only. The reveal does not depend on it, because winit owns
        // WM_ERASEBKGND and a light-background probe showed the pre-paint
        // window is black whatever the class brush says.
        if let Some(h) = window_hwnd(&window) {
            set_window_background_brush(h, self.cfg.theme.background);
        }
        let early_reveal = should_reveal_after_renderer_init(self.cfg.window_state)
            && background_is_dark_enough_to_reveal_early(self.cfg.theme.background);
        if early_reveal {
            window.set_visible(true);
            ws.window_shown = true;
            crate::startup_trace::mark(crate::startup_trace::Phase::WindowRevealed);
        }
        let size = window.inner_size();
        let scale = window.scale_factor() as f32;
        // Guard the synchronous GPU init against a hung
        // graphics driver. `Renderer::new` block_on's wgpu's adapter+device
        // requests on this (event-loop) thread; a wedged driver or GPU reset
        // can make those never return, leaving kettle stuck on an invisible
        // window (kept hidden until the first paint) with no
        // diagnostic — indistinguishable from a crash. A watchdog thread (which
        // only ever touches an `AtomicBool`, so there's no Send/thread-affinity
        // hazard with the GPU objects) turns that infinite hang into a clean,
        // actionable exit. `done` is set the instant init returns — success OR
        // a quick failure — so only a true never-returns hang trips it.
        let gpu_init_done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let gpu_init_done = gpu_init_done.clone();
            // If the watchdog thread can't be spawned, proceed without it
            // rather than failing init.
            let _ = std::thread::Builder::new()
                .name("kettle-gpu-init-watchdog".into())
                .spawn(move || {
                    if gpu_init_timed_out(
                        &gpu_init_done,
                        std::time::Duration::from_secs(GPU_INIT_TIMEOUT_SECS),
                        std::time::Duration::from_millis(100),
                    ) {
                        log::error!(
                            "GPU initialization did not finish within {GPU_INIT_TIMEOUT_SECS}s — \
                             your graphics driver may be hung or unresponsive. Try updating the \
                             GPU driver; on Linux you can force software rendering with \
                             `LIBGL_ALWAYS_SOFTWARE=1` or pick another backend via \
                             `WGPU_BACKEND=gl`. Exiting so kettle doesn't hang invisibly."
                        );
                        // exit() skips destructors, so run_with's guard never
                        // flushes this startup's trace.
                        crate::startup_trace::flush();
                        std::process::exit(1);
                    }
                });
        }
        let init_result = {
            let runtime_tracker = self.runtime_tracker.clone();
            let _phase = runtime_tracker.enter("gpu_init");
            pollster::block_on(Renderer::new_with_display_handle(
                window.clone(),
                event_loop.owned_display_handle(),
                size.width.max(1),
                size.height.max(1),
                scale,
                &self.cfg,
                ws.startup_fonts.take(),
            ))
        };
        // Disarm the watchdog the moment init returns, on BOTH the success and
        // the quick-failure path, so a clean renderer-init error below never
        // gets a spurious "timed out" report behind it.
        gpu_init_done.store(true, std::sync::atomic::Ordering::Release);
        let renderer = match init_result {
            Ok(r) => r,
            Err(e) => {
                log::error!("renderer init failed: {e}");
                ws.mux.kill_children();
                event_loop.exit();
                return;
            }
        };
        crate::startup_trace::mark(crate::startup_trace::Phase::GpuReady);
        log::info!(
            "startup: working set after gpu init {:.1} MiB",
            process_working_set_mb()
        );
        log::info!(
            "startup: window create+setup {create_window_ms:.1}ms, accessibility {a11y_ms:.1}ms,              gpu {:.1}ms (cumulative {:.1}ms)",
            t_startup.elapsed().as_secs_f64() * 1000.0 - create_window_ms - a11y_ms,
            t_startup.elapsed().as_secs_f64() * 1000.0
        );
        // Cache the shared GPU context for open_window (windows 2..N).
        let gpu = renderer.gpu().clone();
        let recovery_proxy = self.proxy.clone();
        gpu.set_recovery_wake(kettle_render::ScreenshotRecoveryWake::new(move || {
            let _ = recovery_proxy.send_event(UserEvent::Wakeup);
        }));
        self.gpu = Some(gpu);
        ws.renderer = Some(renderer);
        ws.startup_cell = None;
        ws.startup_surface = None;
        ws.accessibility = Some(accessibility);
        ws.window = Some(window);
        self.sync_output_frame_budget(ws, true);
        if let Some(theme) = ws.window.as_ref().and_then(|w| w.theme()) {
            self.apply_initial_os_theme_preference(ws, theme);
        }

        let area = self.area(ws);
        let (cols, rows) = self.grid_of(ws, area);
        let geometry = self.pty_geometry_for_grid(ws, cols, rows);

        // CLI `-e cmd` / `-d dir` are consumed ONCE (they seed this window's
        // first tab and must not respawn on restore); take exactly those two
        // fields. The REST of `self.startup` stays intact for the whole
        // session — the explicit-restore gates below (`--tab-handoff` /
        // `--layout` / `--restore`), the save-session layout/restore gating,
        // and reload_config's launch-override re-application all
        // read it later.
        //
        // A wholesale `mem::take(&mut self.startup)` would silently default
        // those later reads, so `--layout` / `--restore` / `--tab-handoff`
        // would never load at startup and live reloads would drop the
        // `-m/-f/-b/-H/-T` overrides. Pinned by
        // `startup_is_not_taken_wholesale` below.
        let cmd_override = self.startup.command.take();
        let cwd_override = self.startup.cwd.take();
        // Resolve the session recording target for this launch: an explicit
        // `--record`/`--record-dir`/`KETTLE_RECORD*` override (in
        // `self.startup.record`) wins; otherwise the persistent `record = on`
        // config key records into `record-dir` (default `<config-dir>/recordings`).
        let dev_record = if let Some(target) = self.startup.record.clone() {
            Some((target, self.startup.record_raw_input))
        } else if self.cfg.record.enabled() {
            // `record-dir` from the config file is NOT shell-expanded, so honor a
            // leading `~`. Unset falls back to `<config-dir>/recordings`, resolved
            // from `Config::default_path()` (the base config dir, never a
            // `--profile` subdir) so it matches the documented default.
            self.cfg
                .record_dir
                .as_deref()
                .map(expand_user_path)
                .or_else(|| {
                    Config::default_path()
                        .and_then(|p| p.parent().map(|dir| dir.join("recordings")))
                })
                .map(|dir| {
                    (
                        kettle_core::record::RecordingTarget::Directory(dir),
                        self.cfg.record_raw_input,
                    )
                })
        } else {
            None
        };
        // Restored sibling windows can drain output as soon as they open.
        if let Some((target, raw)) = dev_record {
            let (cols, rows) = self.grid_of(ws, self.area(ws));
            match crate::dev_record::Recorder::start_target_async(
                &target,
                cols as u16,
                rows as u16,
                raw,
            ) {
                Ok(mut rec) => {
                    log::info!("dev-record: queued asynchronous start for {target:?}");
                    let proxy = self.proxy.clone();
                    rec.set_failure_waker(Arc::new(move || {
                        let _ = proxy.send_event(UserEvent::Wakeup);
                    }));
                    self.recording_start_failed = false;
                    self.recording_error_reported = false;
                    self.recorder = Some(rec);
                    self.sync_recording_state(ws, "");
                }
                Err(e) => {
                    log::warn!("dev-record: could not start recorder for {target:?}: {e}");
                    self.recording_start_failed = true;
                    let body = self
                        .ui_text
                        .notify_body_recording_start_failed(&e.to_string());
                    self.sync_recording_state(ws, &body);
                }
            }
        }
        let restored = if spawned_early {
            true
        } else if has_launch_override {
            let argv = cmd_override.unwrap_or_default();
            let cwd = cwd_override
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned());
            if let Err(e) = ws.mux.new_tab_with_geometry(
                &self.cfg,
                geometry,
                self.waker(),
                &argv,
                cwd.as_deref(),
            ) {
                log::error!("failed to spawn `-e` command: {e}");
                event_loop.exit();
                return;
            }
            true
        } else {
            match restore_plan.as_ref() {
                Some((wins, surfaces)) => {
                    // The theme is config-governed (the config `theme =` line,
                    // with the compile-time default as fallback) and persisted
                    // via `persist_pref`. Restore never applies a
                    // session-stored theme, which would override the
                    // config/default on every restore. The session's `theme`
                    // field is ignored (kept only for back-compat parsing of
                    // older session.json files).
                    // `self.waker()` is the latched waker constructor; a
                    // hand-rolled closure would bypass the wakeup-dedup latch
                    // for every restored pane.
                    // Window 1 of the session restores into THIS window; each
                    // additional saved window opens via open_window(Restore),
                    // which works here because the GPU context was cached just
                    // above. A legacy single-window file normalizes to one
                    // entry.
                    if let Some((first, rest)) = wins.split_first() {
                        let first_session = crate::session::Session {
                            tabs: first.tabs.to_vec(),
                            active: first.active,
                            theme: None,
                            windows: Vec::new(),
                        };
                        let geometries = self.restore_geometries(ws, &first_session);
                        let mk = || self.waker();
                        let outcome =
                            ws.mux
                                .restore_geometry(&first_session, &self.cfg, &geometries, &mk);
                        // A tab that could not be rebuilt is gone from the live
                        // mux, and the next save rewrites the file from it. Keep
                        // the original so a transient failure — a command not on
                        // PATH yet, a fork refused under a process limit — costs
                        // nothing permanent.
                        if outcome.skipped > 0 {
                            self.stash_unrestorable_session(&format!(
                                "{} tab(s) could not be rebuilt",
                                outcome.skipped
                            ));
                        }
                        if !rest.is_empty() {
                            for (sw, surface) in rest.iter().zip(&surfaces[1..]) {
                                let sw = crate::session::SWindow {
                                    tabs: sw.tabs.to_vec(),
                                    active: sw.active,
                                    geometry: surface.geometry(),
                                };
                                let size =
                                    winit::dpi::PhysicalSize::new(surface.size.0, surface.size.1);
                                if self
                                    .open_window(
                                        event_loop,
                                        WindowOpen::Restore(sw),
                                        None,
                                        Some(size),
                                    )
                                    .is_err()
                                {
                                    self.stash_unrestorable_session(
                                        "could not open an additional window",
                                    );
                                    log::warn!(
                                        "session restore: could not open an additional window"
                                    );
                                }
                            }
                            // Focus comes home to window 1 (open_window moves
                            // focused_seq to each window it opens).
                            self.focused_seq = ws.seq;
                        }
                        outcome.restored_any
                    } else {
                        false
                    }
                }
                _ => false,
            }
        };
        if !restored && let Err(e) = ws.mux.new_tab_geometry(&self.cfg, geometry, self.waker()) {
            log::error!("failed to spawn shell: {e}");
            event_loop.exit();
            return;
        }
        if !spawned_early {
            crate::startup_trace::mark(crate::startup_trace::Phase::PaneSpawned);
            crate::startup_trace::note_path(crate::startup_trace::StartupPath::AfterRenderer);
        }
        if ws.cursor_exit_log.enabled
            && let Some(pane_id) = ws.mux.tabs.get(ws.mux.active).map(|tab| tab.focus)
            && let Some(window) = ws.window.as_deref()
        {
            ws.cursor_exit_log.bind(ws.seq, pane_id, window);
        }
        self.resize_all(ws);
        // Start the control server right after the first pane exists and BEFORE
        // the first GPU paint, which can take several seconds on a cold shader
        // cache. The server only needs the pid + a live proxy for its waker, so
        // binding it here makes the agent surface available as soon as the
        // window comes up. A re-resume does not start it again.
        if self.ctl_start == CtlStart::NotReady {
            self.ctl_start = CtlStart::Ready;
        }
        self.ensure_ctl_started();
        // Terminator plugin parity: fire
        // LuaEvent::Startup the first time we have an alive window
        // + at least one pane. Subsequent resumed() calls (Wayland
        // can re-emit) get short-circuited by lua_startup_fired.
        // Drains any LuaCommand the callbacks queued so a
        // `kettle.on('startup', function() kettle.send_text(...) end)`
        // takes effect immediately.
        if !self.lua_startup_fired && self.lua_engine.is_some() && ws.mux.focused().is_some() {
            self.fire_lua_event(ws, crate::LuaEvent::Startup, "startup hook");
            self.lua_startup_fired = true;
        }
        // Top-level script commands precede startup-hook commands in the same
        // FIFO. Run one bounded slice before the first paint; a stalled child
        // retains the exact head message and about_to_wait drives retries.
        let _ = self.poll_pending_lua_commands(ws, event_loop, std::time::Instant::now());
        if should_reveal_before_first_surface_frame(
            self.cfg.window_state,
            ws.window_shown,
            cfg!(target_os = "macos"),
        ) {
            if let Some(w) = &ws.window {
                w.set_visible(true);
            }
            ws.window_shown = true;
            crate::startup_trace::mark(crate::startup_trace::Phase::WindowRevealed);
            log::info!(
                "startup: window revealed before first macOS surface frame, working set {:.1} MiB",
                process_working_set_mb()
            );
        }
        self.start_window_frames(ws);
        // A window that starts hidden presents nothing until it is shown, so
        // its startup trace ends here, without a first frame. Any other
        // window's trace is printed by the first frame it presents.
        if !should_reveal_after_renderer_init(self.cfg.window_state) {
            crate::startup_trace::flush();
        }
        // The first frame is scheduled. The signed-feed worker skips the first
        // launch, applies the shared per-interval throttle, and honors
        // off/notify/auto. A recurring timer re-checks a long-lived window.
        crate::update_check::maybe_spawn_check(
            self.proxy.clone(),
            env!("CARGO_PKG_VERSION"),
            self.startup.update_install.clone(),
            self.cfg.update_policy,
            self.cfg.update_check_interval_hours,
        );
        crate::update_check::spawn_update_check_timer(self.proxy.clone());
        self.request_update_facts(true);
        // Warm the shell-detection cache (wsl.exe /
        // vswhere probes, bounded ~2s each) off the UI thread so the first
        // dropdown open / Ctrl+Shift+N press doesn't pay it.
        kettle_core::term::prewarm_shell_detection();
    }

    fn user_event_inner(&mut self, ws: &mut WindowState, _el: &ActiveEventLoop, ev: UserEvent) {
        ws.reuse_pane_snapshots_once = false;
        match ev {
            #[cfg(target_os = "macos")]
            UserEvent::DockReopen
            | UserEvent::DockReopenReady(_)
            | UserEvent::DockReopenFinished(_) => {}
            #[cfg(target_os = "linux")]
            UserEvent::Wayland => {}
            // Consumed by the outer window-less dispatch before a WindowState
            // is checked out.
            UserEvent::Activation
            | UserEvent::PastedImageEncoded
            | UserEvent::NativeFullscreenExited { .. }
            | UserEvent::AccessibilityActivated { .. }
            | UserEvent::RemoteScanReady
            | UserEvent::MediaRendered
            | UserEvent::LanePlayback
            | UserEvent::UpdateToastClaimed { .. } => {}
            UserEvent::AccessibilityAction { request, .. } => {
                self.handle_accessibility_action(ws, request, _el);
            }
            UserEvent::VideoPreviewReady {
                window_seq,
                pane_id,
                generation,
                candidate,
                preview,
            } => {
                if window_seq != ws.seq {
                    return;
                }
                use crate::window_state::VideoPreviewOutcome;
                let poster = preview.is_some();
                let outcome = ws.finish_pending_video_preview(
                    pane_id,
                    generation,
                    candidate,
                    preview,
                    std::time::Instant::now(),
                );
                log::debug!("video paste receipt for pane {pane_id}: {outcome:?}, poster {poster}");
                if matches!(
                    outcome,
                    VideoPreviewOutcome::Failed | VideoPreviewOutcome::Shown
                ) && let Some(window) = &ws.window
                {
                    window.request_redraw();
                }
            }
            UserEvent::Wakeup => {
                // Semantic terminal events and direct-child lifecycle edges
                // are not paint damage. Drain them before the output-generation
                // gate so a quiet exit is applied even when the window is
                // hidden, minimized, or has cursor blinking disabled.
                if take_window_lifecycle_wakes(ws) {
                    self.drain_events(ws);
                }
                // Apply current visibility/recovery policy, but leave enabled
                // latches closed until redraw is ready to snapshot a frame.
                // This prevents one queued event from reopening the flood gate
                // before the deferred paint budget expires.
                self.sync_output_wake_gate(ws);
                // Wakeups fan out to every window; skip windows whose
                // panes produced no output since their last paint (plain
                // text emits no TermEvent — the generation counter is the
                // only reliable per-window signal).
                if !output_wakeup_needs_paint(
                    || self.window_has_new_output(ws),
                    || self.acknowledge_output_wakes(ws),
                ) {
                    return;
                }
                // Run output triggers before the redraw —
                // a match fires window urgency so the user notices the
                // event even if they're focused on another OS window.
                // Cheap when triggers are empty (which is the default).
                self.run_triggers(ws);
                if output_wakeup_must_quiesce(
                    window_is_render_hidden(ws),
                    ws.frame_recovery.has_pending(),
                    ws.renderer.is_none() || self.gpu.as_ref().is_some_and(|gpu| gpu.is_lost()),
                ) {
                    if ws.mux.panes.values().any(|pane| pane.output_rx.is_some()) {
                        // Rearm before draining. Output racing after this point
                        // owns a new wake; output already queued is consumed by
                        // this nonblocking drain. Reversing the order leaves a
                        // drain/ack race that can strand the next lossless
                        // chunk with no event-loop wake.
                        self.acknowledge_output_wakes(ws);
                        self.drain_recorder_output_once(ws);
                    }
                    // Keep terminal damage pending while the surface is
                    // occluded, minimized, explicitly invisible, or while its
                    // one-shot repair deadline is armed.
                    // Output bursts must not bypass timeout backoff.
                    ws.output_pacer.reset();
                    return;
                }
                // Coalesce rapid PTY-output paints to ~one per
                // frame budget so a non-atomic repaint burst settles before we
                // snapshot the grid. When deferred, `about_to_wait` schedules
                // the flush at the budget deadline so the final frame still
                // paints. Input/cursor paints don't come through here, so
                // typing and the cursor stay immediate.
                let now = std::time::Instant::now();
                // Keystroke ECHO paints immediately. request_redraw is
                // vsync-coalesced, so this can't outpace the display, and the
                // coalescer's job (letting a NON-atomic output burst settle
                // before the snapshot) doesn't apply to a few echoed cells.
                // Routing echo through the WaitUntil deadline (~16ms timer
                // granularity on Windows, frequently late) makes held-key
                // repeat visibly stutter.
                if typed_recently(now, ws.last_typed, TYPING_ECHO_WINDOW) {
                    ws.output_pacer.reset();
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                } else if should_defer_output_paint(
                    now,
                    ws.last_paint,
                    effective_output_budget(ws.output_frame_budget, ws.flood_paints),
                ) {
                    ws.output_pacer.defer();
                } else if let Some(w) = &ws.window {
                    ws.output_pacer.queue_deferred_redraw();
                    w.request_redraw();
                }
            }
            // Consumed by the outer process-level debounce dispatch.
            UserEvent::ReloadConfig => {}
            UserEvent::RemoteCommand => self.drain_remote_commands(ws),
            UserEvent::Ctl => self.drain_ctl(ws, _el),
            UserEvent::UpdateAvailable { tag, notify } => {
                let _ = (tag, notify);
                self.request_update_facts(true);
            }
            UserEvent::UpdateFacts(facts) => {
                if self.update_facts_refresh.complete() {
                    self.apply_update_facts(ws, *facts);
                } else {
                    self.request_update_facts(true);
                }
            }
            UserEvent::UpdateInstalled {
                tag,
                staged,
                first_time,
                notify,
            } => {
                self.request_update_facts(true);
                let _ = (tag, staged, first_time, notify);
            }
            UserEvent::UpdateFailed { tag, notify } => {
                self.request_update_facts(true);
                let _ = (tag, notify);
            }
            UserEvent::UpdateRestartPending { installed } => {
                // Update facts project a foreground toast. Log each
                // version once rather than on every hourly check.
                if self.logged_restart_pending.as_deref() != Some(installed.as_str()) {
                    log::info!(
                        "kettle {installed} is installed; this process runs {} until it restarts",
                        env!("CARGO_PKG_VERSION")
                    );
                    self.logged_restart_pending = Some(installed);
                }
                self.request_update_facts(true);
            }
            UserEvent::UpdateCheckTick => {
                crate::update_check::maybe_spawn_check(
                    self.proxy.clone(),
                    env!("CARGO_PKG_VERSION"),
                    self.startup.update_install.clone(),
                    self.cfg.update_policy,
                    self.cfg.update_check_interval_hours,
                );
                // Another process may have installed, dismissed or failed.
                self.request_update_facts(true);
            }
            // Every Dock row maps onto an action kettle already has, so this
            // reuses the keybind path wholesale — including NewWindow's
            // fall-back to a new tab when a window cannot be created.
            #[cfg(target_os = "macos")]
            UserEvent::DockCommand(command) => {
                if command == crate::macos_dock::DockCommand::NewTab
                    && let Some(w) = &ws.window
                {
                    w.set_visible(true);
                    w.set_minimized(false);
                    w.focus_window();
                }
                self.handle_action(ws, command.action(), _el);
            }
        }
    }

    fn window_event_inner(
        &mut self,
        ws: &mut WindowState,
        event_loop: &ActiveEventLoop,
        event: WindowEvent,
    ) {
        let preserves_menu_redraw_hint = matches!(
            &event,
            WindowEvent::RedrawRequested | WindowEvent::CursorMoved { .. }
        ) && ws.context_menu.is_some();
        if !preserves_menu_redraw_hint {
            ws.reuse_pane_snapshots_once = false;
        }
        if let (Some(adapter), Some(window)) = (&mut ws.accessibility, &ws.window) {
            adapter.process_event(window, &event);
        }
        match event {
            WindowEvent::CloseRequested => {
                // The titlebar ✕ and Alt+F4 ask the window manager to destroy a
                // window; they are not Kettle commands. Ordinary applications
                // veto that only for unsaved document state, and a terminal has
                // no authoritative dirty bit ("a process exists" is not "work
                // would be lost"). A veto would also make the gesture
                // nondeterministic, and because `shell_idle()` reports busy for
                // any pane without OSC 133 integration, EVERY multi-pane window
                // would prompt. So this gesture closes.
                //
                // `ask-before-closing = always` is the opt-in for people who
                // want the question here, and the only policy that still asks
                // it. The keybind close actions (`ClosePane`, `CloseTab`,
                // `CloseWindow`) are Kettle commands, not OS requests, and keep
                // the full policy. Ask BEFORE any of the teardown below, so the
                // session is not saved and the recorder not finished while the
                // user is still deciding.
                if self.cfg.ask_before_closing == kettle_config::AskBeforeClosing::Always {
                    let (scope, busy) = window_close_scope(ws);
                    if self.confirm_close(
                        ws,
                        scope,
                        busy,
                        ConfirmAction::CloseWindow {
                            drop_panes: DropPanes::No,
                        },
                    ) {
                        return;
                    }
                }
                self.close_window_now(ws, DropPanes::No);
            }
            WindowEvent::Resized(size) => {
                self.confirm_cancel_press(ws);
                self.confirm_scroll(ws, 0, true);
                self.sync_output_frame_budget(ws, false);
                // Minimizing a window delivers Resized(0, 0) on Windows.
                // Reconfiguring the surface + `resize_all` to a 0×0 area would
                // collapse every PTY to a 1×1 grid (`grid_of`'s `.max(1)`) and
                // fire a SIGWINCH storm that reflows/redraws every TUI in every
                // pane, then reflows them all back on restore. Skip a
                // degenerate size entirely: keep the last good grid so PTYs
                // hold their real dimensions; the genuine restore carries the
                // true non-zero size and reflows once.
                if size.width == 0 || size.height == 0 {
                    ws.dpi_resize_surface_suspended = true;
                    return;
                }
                // AppKit can briefly restore its stale standard frame. Wait
                // for the window-local restore before reflowing terminal grids.
                if ws
                    .native_fullscreen
                    .as_ref()
                    .is_some_and(|native| native.pending())
                {
                    ws.dpi_resize_surface_suspended = true;
                    return;
                }
                ws.dpi_resize_surface_suspended = false;
                // The Metal layer stretches its last frame to the new size
                // until the resize's frame presents, and that frame may have
                // to retry. The cursor patch would keep blinking where the
                // cell was.
                if ws
                    .renderer
                    .as_ref()
                    .is_none_or(|r| r.surface_size() != (size.width, size.height))
                {
                    Self::hide_cursor_layer(ws);
                }
                let renderer_ready = self.dpi_resize_renderer_ready(ws);
                if ws.dpi_resize.on_event(DpiResizeEvent::Resized {
                    width: size.width,
                    height: size.height,
                    renderer_ready,
                }) == DpiResizeAction::ApplyLayout
                {
                    self.commit_window_resize(ws, size.width, size.height);
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                // The patch was drawn at the old scale, and the new-scale frame
                // may wait for the paired resize.
                Self::hide_cursor_layer(ws);
                self.sync_output_frame_budget(ws, true);
                // Apply the new DPI scale so text follows >100% Windows scaling
                // and rescales when dragged to a different-DPI monitor.
                // set_scale re-derives physical font metrics + cell size. Do
                // not re-grid yet: on Windows this event precedes the
                // SetWindowPos-driven Resized, so the physical size is still
                // from the old monitor. The coalescer commits once at the
                // following usable Resized; about_to_wait is the fallback for
                // backends that do not emit one.
                ws.dpi_resize.on_event(DpiResizeEvent::ScaleFactorChanged);
                ws.frame_recovery.expedite(std::time::Instant::now());
                if !ws.frame_recovery.renderer_rebuild_pending()
                    && let Some(r) = ws.renderer.as_mut()
                {
                    r.set_scale(scale_factor as f32);
                }
            }
            WindowEvent::ThemeChanged(theme) => {
                self.apply_os_theme_preference(ws, theme);
            }
            WindowEvent::ModifiersChanged(m) => {
                #[cfg(not(target_os = "macos"))]
                {
                    ws.mods = m.state();
                }
                #[cfg(target_os = "macos")]
                {
                    use winit::keyboard::ModifiersKeyState;

                    ws.macos_raw_mods = m.state();
                    ws.macos_left_option_pressed = m.lalt_state() == ModifiersKeyState::Pressed;
                    ws.macos_right_option_pressed = m.ralt_state() == ModifiersKeyState::Pressed;
                    refresh_macos_modifiers(ws, self.cfg.macos_option_as_alt);
                }
                // Modifier change can flip the URL hover affordance from
                // text-I-beam to pointing-hand without the mouse moving
                // (Ctrl held = "this click would open"). Re-sync the
                // cursor icon so the affordance updates the moment Ctrl
                // is pressed/released over a link.
                self.sync_cursor_icon(ws);
            }
            // Terminator parity: winit CursorLeft/Entered events transition the
            // detach FSM. CursorLeft → DraggingOutside; CursorEntered →
            // DraggingInside (the user brought the cursor back, which cancels
            // the cross-window flow). Mouse-down arming on the tab bar enters
            // the FSM, and a transition on `Idle` is a harmless no-op.
            //
            // The torn window streams `Moved` during the OS move loop (Windows:
            // WM_WINDOWPOSCHANGED inside the NC modal loop; X11:
            // ConfigureNotify from the WM move, both verified). Approximate the
            // live cursor as frame origin + grab offset and drive the re-dock
            // hit-test against every sibling window's tab band.
            WindowEvent::Moved(pos) => {
                self.sync_output_frame_budget(ws, false);
                if let Some(started) = ws
                    .native_caption_drag
                    .as_ref()
                    .and_then(|drag| drag.take_start())
                    && self.torn_drag.is_none()
                    && crate::native_drag::may_dock_caption(
                        self.cfg.detachable_tabs,
                        !matches!(self.cfg.tab_bar, TabBarMode::Off),
                        ws.mux.tabs.len(),
                        self.pointer_modal_open(ws),
                    )
                    && let Some(cursor) = cursor_screen_pos()
                    && let Some(window) = ws.window.as_ref()
                    && !window.is_maximized()
                    && window.fullscreen().is_none()
                {
                    let origin = drag_position(window, pos);
                    self.torn_drag = Some(TornDrag {
                        seq: ws.seq,
                        carrier: ws.seq,
                        started,
                        grab: (cursor.0 - origin.0, cursor.1 - origin.1),
                        dock: None,
                        native: true,
                        caption: true,
                        saw_move: false,
                        last_signal: std::time::Instant::now(),
                        hwnd: window_hwnd(window),
                        signal_cursor: Some(cursor),
                    });
                }
                let Some(td) = self.torn_drag.as_mut() else {
                    return;
                };
                if td.seq != ws.seq {
                    return;
                }
                td.saw_move = true;
                td.last_signal = std::time::Instant::now();
                let grab = td.grab;
                // Frame + grab approximates the pointer, but the move
                // loop's anchor drifts from `grab` by however far the
                // pointer slid between press/tear and loop start — a
                // session recording measured 55-86px under Mutter, enough
                // to miss the whole band. The LIVE cursor is one call away
                // on Windows (GetCursorPos) AND on X11 (QueryPointer via
                // the side connection); the approximation remains the
                // fallback for a failed query. Each real-cursor + `Moved`
                // pair also refreshes `signal_cursor`, the rescue tick's
                // travel-without-`Moved` demotion reference.
                let origin = ws
                    .window
                    .as_deref()
                    .map(|window| drag_position(window, pos))
                    .unwrap_or((f64::from(pos.x), f64::from(pos.y)));
                let approx = (origin.0 + grab.0, origin.1 + grab.1);
                let real = cursor_screen_pos();
                if let Some(cur) = real {
                    td.signal_cursor = Some(cur);
                }
                let cursor = real.unwrap_or(approx);
                let torn_hwnd = ws.window.as_deref().and_then(window_hwnd);
                self.update_dock_target(cursor, torn_hwnd, None);
            }
            WindowEvent::CursorLeft { .. } => {
                if let Some(dialog) = ws.confirm_dialog.as_mut() {
                    dialog.armed_press = None;
                }
                let prev = std::mem::take(&mut ws.detach_drag);
                ws.detach_drag = prev.on_cursor_leave_window();
                if ws.selecting
                    && let Some(window) = &ws.window
                {
                    let size = window.inner_size();
                    ws.selection_autoscroll_edge = selection_leave_edge(
                        ws.selection_dragged,
                        ws.cursor.x as f32,
                        ws.cursor.y as f32,
                        size.width as f32,
                        size.height as f32,
                    );
                }
                // Pointer left the window → drop the scrollbar hover so
                // the overlay bar relaxes to its dim state.
                ws.scrollbar_hover = false;
                ws.hovered_close_idx = None;
                ws.hovered_new_tab = false;
                ws.hovered_new_tab_menu = false;
                ws.search.hovered_control = None;
                // No card is under a pointer that left.
                ws.native_pointer = None;
                ws.card_hover = None;
                ws.card_settle_wake = None;
                if let Some(receipt) = ws.media_paste_receipt.as_mut() {
                    receipt.set_hover(false, std::time::Instant::now());
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            WindowEvent::CursorEntered { .. } => {
                let prev = std::mem::take(&mut ws.detach_drag);
                ws.detach_drag = prev.on_cursor_reenter_window();
                ws.selection_autoscroll_edge = 0;
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                ws.cursor = position;
                ws.native_pointer = Some(position);
                ws.selection_autoscroll_edge = 0;
                // A press the lane took owns the pointer's motion until its
                // release: it resizes the lane or pans its item, and nothing
                // else sees it.
                if self.resize_preview_lane(ws) || self.drag_preview_lane(ws) {
                    self.sync_cursor_icon(ws);
                    return;
                }
                self.update_search_hover(ws);
                if ws.selecting {
                    note_selection_drag_motion(ws);
                }
                // Client pointer events do NOT reach the torn window while the
                // OS moves it (Windows: NC modal loop; X11: the WM holds an
                // active pointer grab). The first CursorMoved after movement is
                // therefore the post-drop signal on platforms without Windows'
                // synthesized release (which arrives first there and wins).
                // Guards: a short post-handoff blackout absorbs a stray client
                // motion racing the WM actually taking the grab, and the latch
                // is REVALIDATED against the torn window's final resting
                // position — a WM-cancelled move (Esc) snaps the window back to
                // its origin, where frame+grab no longer hits the latched band,
                // so the cancel abandons instead of committing. Then fall
                // through — this event is real input for the now-free window.
                if let Some(td) = self.torn_drag.as_ref()
                    && td.seq == ws.seq
                    && td.native
                    && td.saw_move
                    && (!td.caption || !primary_button_physically_held())
                    && td.started.elapsed() >= std::time::Duration::from_millis(300)
                {
                    let commit = self.revalidate_dock_latch(ws);
                    self.finalize_torn_drag(ws, commit);
                    // A committed merge empties this window — it is closing;
                    // don't process the event against an empty mux.
                    if self.window_close_pending(ws.seq) {
                        return;
                    }
                }
                // Self-healing demotion. The drag_window() call succeeded as an
                // API call, but the WM never actually took the move (no Moved
                // has arrived) and the capture holder is still streaming
                // motion. An X11 WM that ignores _NET_WM_MOVERESIZE would
                // otherwise leave the torn window frozen mid-air with no path
                // to carry it.
                if let Some(td) = self.torn_drag.as_mut()
                    && td.native
                    && !td.saw_move
                    && td.carrier == ws.seq
                    && td.started.elapsed() >= std::time::Duration::from_millis(400)
                {
                    log::info!("tear-off: WM never took the drag; demoting to manual follow");
                    td.native = false;
                }
                // Manual-follow fallback. The native handoff failed, so the
                // capture holder (the tear's source window, or the dragged
                // window itself for a lone-tab whole-window drag) carries the
                // torn window. Reposition it from this cursor stream and drive
                // the re-dock hit-test. Gated on the CARRIER, so only the
                // capture holder's stream drives the follow. Without the gate,
                // stale tracking would hijack every window's cursor stream and
                // keep refreshing the failsafe forever.
                if self.follow_torn_drag(ws) {
                    return;
                }
                // An editor drag that started inside the search bar keeps the
                // motion; otherwise the grid above the bar stays live for
                // hover, selection, and mouse reporting, matching the ctl path.
                if search_pointer_route(ws.search.open, None, ws.search.pointer_captured())
                    == SearchPointerRoute::Bar
                {
                    self.show_mouse_cursor(ws);
                    self.sync_cursor_icon(ws);
                    self.search_mouse_drag(ws);
                    return;
                }
                // Dragging a split divider — recompute the
                // addressed split's ratio from the cursor and apply. The split
                // rect is re-fetched from the live seams so a mid-drag layout
                // change (a pane elsewhere closing) can't desync the math; if
                // the split has vanished, cancel the drag.
                if self.drag_split_divider(ws) {
                    return;
                }
                // Any real mouse movement undoes the hide-while-typing
                // state. Sub-pixel movements that winit *might* coalesce
                // are fine to ignore — the next "real" motion will fire.
                self.show_mouse_cursor(ws);
                let media_receipt_hovered = self.update_media_paste_receipt_hover(ws);
                self.sync_cursor_icon_with_media_receipt_hover(ws, media_receipt_hovered);
                // Terminator menu UX, hover-to-highlight: hovering a
                // context-menu row highlights it immediately, matching
                // GTK/NSMenu/Win32 menu conventions.
                if ws.context_menu.is_some() {
                    self.update_menu_highlight_from_cursor(ws);
                }
                // Terminator parity, terminatorlib/config.py:73
                // `focus = sloppy`: focus-follows-mouse. The pane
                // under the cursor becomes focused on every cursor
                // movement (vs default `click` mode where click is
                // required). `system` is treated like `click` for
                // kettle — winit doesn't expose the OS-level focus
                // policy.
                if matches!(self.cfg.focus, kettle_config::FocusMode::Sloppy)
                    && !ws.tab_drag_active
                    && !ws.selecting
                    && ws.scrollbar_drag_offset.is_none()
                    // Don't let focus-follows-mouse
                    // reassign pane focus while a modal is open — typing into
                    // search/palette while the cursor drifts over another pane
                    // would otherwise silently steal focus.
                    && !self.any_modal_open(ws)
                {
                    let area = self.area(ws);
                    let pre = self.focus_key(ws);
                    ws.mux
                        .focus_at(area, ws.cursor.x as f32, ws.cursor.y as f32);
                    self.note_focus_change(ws, pre);
                }
                // Drag-to-reorder tabs (kitty / iTerm2 / Ghostty parity), on
                // every tab bar position: once a left press in the bar has
                // moved past the click slop, the active tab follows the
                // segment under the cursor.
                self.promote_tab_drag_if_needed(ws);
                if ws.tab_drag_active
                    && ws.tab_drag_press.is_none()
                    && self.reorder_active_tab_for_cursor(ws)
                    && let Some(w) = &ws.window
                {
                    w.request_redraw();
                }
                // Terminator parity: a titlebar press that has since moved far
                // enough is a pane drag. Recompute the drop target every move
                // from the CURRENT layout rather than caching it at press time
                // — a pane closing mid-drag reshuffles every rect, and a stale
                // target would highlight a pane that is no longer there.
                if ws.pane_drag.is_some() {
                    let area = self.area(ws);
                    let (cx, cy) = (ws.cursor.x as f32, ws.cursor.y as f32);
                    let (ox, oy) = ws.pane_drag.as_ref().map(|d| d.origin).unwrap_or((cx, cy));
                    let (dx, dy) = (cx - ox, cy - oy);
                    let live = ws.pane_drag.as_ref().is_some_and(|d| d.live)
                        || (dx * dx + dy * dy).sqrt() > PANE_DRAG_THRESHOLD_PX;
                    let moving = ws.pane_drag.as_ref().map(|d| d.pane);
                    // Computed before the &mut borrow below: `pane_rect_at`
                    // needs `&ws.mux` and the write needs `&mut ws.pane_drag`.
                    let target = if live {
                        ws.mux.pane_rect_at(area, cx, cy).and_then(|(id, rect)| {
                            // A pane cannot be dropped onto itself: the tree
                            // half refuses it, so offering the hint would
                            // promise a move that never happens.
                            (Some(id) != moving)
                                .then(|| {
                                    crate::mux::pane_drop_zone(rect, cx, cy)
                                        .map(|(dir, before)| (id, rect, dir, before))
                                })
                                .flatten()
                        })
                    } else {
                        None
                    };
                    if let Some(drag) = ws.pane_drag.as_mut() {
                        let changed = drag.live != live
                            || drag.target.map(|t| t.0) != target.map(|t| t.0)
                            || drag.target.map(|t| (t.2, t.3)) != target.map(|t| (t.2, t.3));
                        drag.live = live;
                        drag.target = target;
                        if changed && let Some(w) = &ws.window {
                            w.request_redraw();
                        }
                    }
                }
                // Drive the detach FSM from the press origin.
                // Distance decides click-vs-drag; WINDOW-BOUNDS CONTAINMENT
                // decides inside/outside — Windows' SetCapture keeps
                // streaming CursorMoved (with out-of-client coordinates)
                // while the button is held but suppresses CursorLeft, so
                // position is the reliable outside signal. The
                // CursorLeft/Entered arms above remain as supplementary
                // signals for platforms that do deliver them mid-drag.
                if let Some((ox, oy)) = ws.drag_press {
                    let (cx, cy) = (ws.cursor.x as f32, ws.cursor.y as f32);
                    let next = std::mem::take(&mut ws.detach_drag).on_mouse_move(cx - ox, cy - oy);
                    let (sw, sh) = ws
                        .renderer
                        .as_ref()
                        .map(|r| {
                            let (w, h) = r.surface_size();
                            (w as f32, h as f32)
                        })
                        .unwrap_or((800.0, 600.0));
                    let outside = cx < 0.0 || cy < 0.0 || cx >= sw || cy >= sh;
                    ws.detach_drag = if outside {
                        next.on_cursor_leave_window()
                    } else {
                        next.on_cursor_reenter_window()
                    };
                    // Tear the moment the cursor crosses the band threshold,
                    // not at release (Chromium-style). The torn window appears
                    // live under the cursor and rides the OS move loop from
                    // here.
                    if self.maybe_tear_off(ws, event_loop) {
                        return;
                    }
                }
                {
                    // Motion. `MOUSE_NO_BUTTON` is xterm's code 3, which with
                    // the motion bit is the `CSI < 35 ; x ; y M` report DEC
                    // 1003 exists to deliver; `send_mouse` decides per mode
                    // whether this motion is reportable at all, so 1002 still
                    // needs a held button and 1000 still gets nothing.
                    if !self.pointer_modal_open(ws)
                        && self.send_mouse(ws, input::MOUSE_NO_BUTTON, true, true)
                    {
                        return;
                    }
                }
                if let Some(grab) = ws.scrollbar_drag_offset {
                    let area = self.area(ws);
                    let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
                    self.scrollbar_at(ws, area, px, py, false, Some(grab));
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Overlay-scrollbar hover-brighten. Update the focused
                // pane's hover flag and repaint only on the enter/leave edge so it
                // costs nothing while the pointer is elsewhere.
                {
                    let area = self.area(ws);
                    let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
                    let hover = self.scrollbar_zone_contains(ws, area, px, py);
                    if hover != ws.scrollbar_hover {
                        ws.scrollbar_hover = hover;
                        if let Some(w) = &ws.window {
                            w.request_redraw();
                        }
                    }
                }
                if ws.selecting {
                    let area = self.area(ws);
                    self.update_selection(ws, area);
                }
                if (ws.selecting
                    || !ws.links.is_empty()
                    || ws.confirm_dialog.is_some()
                    || ws.preview_overlay.is_some())
                    && let Some(w) = &ws.window
                {
                    w.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button,
                ..
            } => {
                ws.resume_native_pointer();
                // A fresh press while torn-drag tracking is live. For a NATIVE
                // drag any client press means the tracking is stale (the OS
                // modal loop / WM grab swallows presses mid-drag) — abandon,
                // never merge on a click. A LEFT press during manual-follow is
                // equally impossible mid-gesture (left is held the whole
                // drag) — stale, abandon. But an OTHER-button press routed to
                // the manual-follow capture holder is live mid-drag input:
                // swallow it (Chromium swallows stray presses during a tab
                // drag) instead of killing the gesture.
                if let Some(td) = self.torn_drag.as_ref() {
                    if td.native || button == MouseButton::Left {
                        self.finalize_torn_drag(ws, false);
                    } else if td.carrier == ws.seq {
                        return;
                    }
                }
                // Forward Back / Forward (buttons 8 / 9) to a mouse-tracking
                // app rather than dropping them. No local UI meaning, so they
                // no-op when tracking is off. The same `modal_swallows_pointer`
                // gate as L/M/R + wheel keeps SGR out of a tracking TUI
                // *behind* a palette/settings/… dialog. A lone context menu
                // isn't a modal here.
                // A press of any button is the pointer's: focus is no longer
                // on a card.
                if ws.card_focus.take().is_some() {
                    ws.accessibility_pending = true;
                }
                if let Some(sgr) = extra_mouse_sgr(button) {
                    // Any press ends a lane's drag or edge drag, as below.
                    self.end_lane_resize(ws);
                    ws.lane_drag = None;
                    // One away from a lane holding the keyboard gives it back.
                    self.leave_preview_focus_for_press(ws);
                    // A press of a button still noted as held means its
                    // release went elsewhere: its program gets it now. The
                    // press is Kettle's until a program receives it.
                    self.release_held_buttons(ws, |held| held.button == sgr);
                    ws.held_buttons.pressed(sgr, None, (0, 0));
                    if self.confirm_mouse_press(ws, event_loop, sgr) {
                        return;
                    }
                    // A lone context menu must swallow the side-button press
                    // too. Dismiss the menu and DON'T forward.
                    // `modal_swallows_pointer` returns false for a lone menu,
                    // so without this a Back/Forward click would leak SGR to
                    // the tracking app *behind* the menu AND leave the menu
                    // open (every other button dismisses it).
                    if ws.context_menu.is_some() {
                        ws.context_menu = None;
                        if let Some(w) = &ws.window {
                            w.request_redraw();
                        }
                        return;
                    }
                    // A preview lane owns every press over it; a side button
                    // does nothing there.
                    let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
                    if !self.press_preview_lane(ws, px, py, sgr)
                        && !modal_swallows_pointer(
                            self.pointer_modal_open(ws),
                            ws.context_menu.is_some(),
                        )
                    {
                        self.report_mouse(ws, sgr, true, false);
                    }
                    return;
                }
                let bcode = match button {
                    MouseButton::Left => 0,
                    MouseButton::Middle => 1,
                    MouseButton::Right => 2,
                    _ => return,
                };
                // Any press while a lane's drag is held means that drag's
                // release went elsewhere, or another gesture takes the
                // pointer: the drag is over. Likewise a primary press while a
                // card still holds one: the card's gesture is over.
                self.end_lane_resize(ws);
                ws.lane_drag = None;
                self.leave_preview_focus_for_press(ws);
                if bcode == 0 {
                    ws.card_press = None;
                }
                // Likewise a program still noted as holding this button gets
                // the release it missed, and the press is Kettle's own
                // gesture until a program receives it.
                self.release_held_buttons(ws, |held| held.button == bcode);
                ws.held_buttons.pressed(bcode, None, (0, 0));
                if self.confirm_mouse_press(ws, event_loop, bcode) {
                    return;
                }
                // Context menu: if the menu is open, a left-click
                // either fires the row that was hit or — if the click landed
                // outside the panel — closes the menu. Middle-click outside
                // dismisses too; only right-click falls through so it can
                // reopen/relocate the menu.
                if ws.context_menu.is_some()
                    && let Some(click) = self.context_menu_click_action(ws, bcode)
                {
                    // Route through the shared sink. It closes the menu per
                    // leaf and keeps it open for DrillIntoSubmenu, so clearing
                    // `ws.context_menu` before the dispatch would turn submenu
                    // clicks into plain dismissals.
                    self.dispatch_context_menu_click(ws, click, event_loop);
                    return;
                }
                if ws.context_menu.is_some() && bcode != 2 {
                    // Left/middle outside the panel — dismiss without firing
                    // anything (matches every modern menu).
                    ws.context_menu = None;
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Search controls are real click targets inside the reserved
                // lane; every button inside the lane stays there. A press above
                // the lane is ordinary grid input: the bar is not an overlay,
                // so selecting, opening links, and mouse reporting keep working
                // while a query is being typed (Terminator parity).
                if search_pointer_route(
                    ws.search.open,
                    Some(self.cursor_in_search_bar(ws)),
                    ws.search.dragging_editor,
                ) == SearchPointerRoute::Bar
                {
                    if bcode == 0 {
                        self.search_mouse_press(ws);
                        if let Some(window) = &ws.window {
                            window.request_redraw();
                        }
                    }
                    return;
                }
                // The settings overlay is mouse-driven (click-to-cycle).
                // Left-click a field cycles its value forward, right-click
                // backward; clicking a category tab switches category; a click
                // outside the panel closes settings. Handled before the generic
                // modal swallow below so the click actually does something.
                if ws.settings_nav.is_some()
                    && (bcode == 0 || bcode == 2)
                    && self.settings_mouse(ws, if bcode == 2 { -1 } else { 1 }, true)
                {
                    return;
                }
                // With any *other* modal open
                // (palette / ssh / settings / layout-picker / hint / confirm
                // dialog / inline title-edit / vi copy-mode) the click must be
                // consumed. Otherwise it falls through to the
                // tab-bar / pane-focus / mouse-tracking logic below, switching
                // tabs and injecting mouse events into the terminal *behind* a
                // dialog that looks like it has focus. The context menu is
                // excluded (handled + returned above; a right-click below
                // relocates it), and so is the search bar, whose lane was
                // routed above and whose grid stays live.
                if ws.preview_overlay.is_some() && !self.non_search_modal_open(ws) {
                    self.press_preview_lane(ws, ws.cursor.x as f32, ws.cursor.y as f32, bcode);
                    return;
                }
                if modal_swallows_pointer(self.pointer_modal_open(ws), ws.context_menu.is_some()) {
                    return;
                }
                let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
                // A preview lane owns every press over it, so the terminal
                // beside it never gets one.
                if self.press_update_toast(ws, px, py, bcode) {
                    return;
                }
                if self.press_preview_lane(ws, px, py, bcode) {
                    return;
                }
                // An inline card takes a primary press before anything else
                // in the window does; its release opens the card's item.
                if self.press_card(ws, px, py, bcode) {
                    return;
                }
                // Tab-bar interactions (left = switch / close-✕ / new-+;
                // middle = close that tab).
                let bar = self.tab_bar(ws);
                let in_bar = |r: kettle_render::Rect4, px: f32, py: f32| {
                    px >= r.0 && px < r.0 + r.2 && py >= r.1 && py < r.1 + r.3
                };
                if tab_bar_pointer_region_contains(&bar, px, py) && (bcode == 0 || bcode == 1) {
                    if bcode == 0 && bar.new_tab_menu.2 > 0.0 && in_bar(bar.new_tab_menu, px, py) {
                        // The `▾` dropdown — open the shell chooser
                        // anchored at the arrow's bottom-left. Checked BEFORE the
                        // `+` so the arrow region isn't swallowed by the button.
                        let (ax, ay, _, ah) = bar.new_tab_menu;
                        self.open_new_tab_menu(ws, ax, ay + ah);
                    } else if bcode == 0 && in_bar(bar.new_tab, px, py) {
                        let area = self.area(ws);
                        let (cols, rows) = self.grid_of(ws, area);
                        let geometry = self.pty_geometry_for_grid(ws, cols, rows);
                        // Log a `+`-button new-tab spawn failure rather than
                        // swallowing it, and fire TabAdd on success so Lua
                        // `TabAdd` and dev-record see the new tab.
                        let pre = self.focus_key(ws);
                        match ws.mux.new_tab_geometry(&self.cfg, geometry, self.waker()) {
                            Ok(()) => {
                                self.note_focus_change(ws, pre);
                                self.fire_tab_add_event(ws);
                            }
                            Err(e) => log::warn!("could not open a new tab (+ button): {e}"),
                        }
                    } else if bcode == 0
                        && bar.scroll_left.2 > 0.0
                        && in_bar(bar.scroll_left, px, py)
                    {
                        // `‹` overflow arrow — step to an earlier tab
                        // (scrolls it into view via tab_strip_layout).
                        let pre = self.focus_key(ws);
                        ws.mux.prev_tab();
                        self.note_focus_change(ws, pre);
                    } else if bcode == 0
                        && bar.scroll_right.2 > 0.0
                        && in_bar(bar.scroll_right, px, py)
                    {
                        let pre = self.focus_key(ws);
                        ws.mux.next_tab();
                        self.note_focus_change(ws, pre);
                    } else if let Some(seg) = bar.segments.iter().find(|s| in_bar(s.rect, px, py)) {
                        let close = bcode == 1 || in_bar(seg.close, px, py);
                        if close {
                            // Both gestures that land here — the ✕ button and
                            // middle-click — destroy as much as
                            // `Action::CloseTab` and so must ask the same
                            // question. They can also target a tab that is NOT
                            // the focused one, so the prompt names the tab by a
                            // pane it holds rather than by the focused index.
                            let (panes, busy) = tab_close_scope(ws, seg.idx);
                            if let Some(anchor) =
                                Some(ws.mux.tab_anchor_panes(seg.idx)).filter(|a| !a.is_empty())
                                && self.confirm_close(
                                    ws,
                                    panes,
                                    busy,
                                    ConfirmAction::CloseTabHolding(anchor),
                                )
                            {
                                return;
                            }
                            // Closing a tab (middle-click or
                            // ✕) can shift focus to a different tab
                            // (`reap_tabs`'s bookkeeping).
                            // Treat it like any other focus-changing
                            // action so the cursor on the now-active
                            // tab lands visible immediately.
                            let pre = self.focus_key(ws);
                            // Fire TabClose so plugins see
                            // the ✕-click close the same as Action::CloseTab.
                            let closing_idx = seg.idx;
                            if ws.mux.close_tab_at(seg.idx) {
                                // Save the (empty) session
                                // before exit so next launch starts
                                // fresh rather than restoring the
                                // *previous* multi-tab state. Other
                                // exit paths (Action::CloseTab on the
                                // last tab, WindowEvent::CloseRequested)
                                // save too.
                                self.fire_tab_close_event(ws, closing_idx);
                                self.save_session(ws);
                                self.request_window_close(ws.seq);
                                return;
                            }
                            self.fire_tab_close_event(ws, closing_idx);
                            self.note_focus_change(ws, pre);
                        } else {
                            let pre = self.focus_key(ws);
                            ws.mux.active = seg.idx;
                            ws.mux.touch_active_tab_seen();
                            self.note_focus_change(ws, pre);
                            // Arm the drag-to-reorder
                            // handler so a subsequent CursorMoved
                            // event with the left button still held
                            // can swap the active tab toward the
                            // cursor. Cleared in the Released arm
                            // below. Only on bare left-click (bcode 0
                            // == left, not middle / close).
                            if bcode == 0 {
                                ws.tab_drag_active = true;
                                ws.tab_drag_press = Some((px, py));
                                ws.tab_pressed_idx = Some(seg.idx);
                                // Arm the detach FSM alongside the in-window
                                // reorder, for a LONE tab too. Dragging it past
                                // the band threshold moves the whole window
                                // (Chromium semantics), which is how a torn-off
                                // window re-docks into a sibling.
                                if self.cfg.detachable_tabs {
                                    ws.detach_drag =
                                        crate::detach::DragState::on_mouse_down_on_tab(seg.idx);
                                    ws.drag_press = Some((px, py));
                                } else {
                                    ws.detach_drag = crate::detach::DragState::default();
                                    ws.drag_press = None;
                                }
                            }
                        }
                    }
                    self.resize_all(ws);
                    if let Some(win) = &ws.window {
                        win.request_redraw();
                    }
                    return;
                }
                let area = self.area(ws);
                // The receipt is chrome over terminal cells. The primary
                // button acts; every other button is consumed without acting
                // so hidden terminal content cannot receive the press.
                if self.activate_media_paste_receipt_at(ws, px, py, bcode) {
                    return;
                }
                // The detached card covers terminal cells but is not a shell
                // editor. A press dismisses it and stops here so selecting text,
                // opening a link, or forwarding mouse tracking cannot act on
                // content the card hid.
                let completion_hit = {
                    let overlay = self.overlay(ws, true);
                    completion_overlay_hit_test(
                        overlay.completion.as_ref(),
                        self.menu_cell(ws),
                        (px, py),
                    )
                };
                if completion_hit {
                    if let Some(pane_id) = ws.mux.active_focus()
                        && let Some(pane) = ws.mux.panes.get(&pane_id)
                    {
                        pane.term.clear_completion();
                    }
                    if let Some(window) = &ws.window {
                        window.request_redraw();
                    }
                    return;
                }
                // Ctrl/Cmd + left-click opens a hyperlink under the cursor.
                //
                // Terminator parity, terminatorlib/config.py:120
                // `link_single_click`: when true, single-click (no
                // modifier) is enough to open URLs. Default keeps
                // kettle's Ctrl-click guard so accidental drags don't
                // navigate.
                let url_modifier =
                    self.cfg.link_single_click || ws.mods.control_key() || ws.mods.super_key();
                if bcode == 0
                    && url_modifier
                    && let Some(uri) = self.link_at_cursor(ws).map(|l| l.uri.clone())
                {
                    // Route through helper so the pane's origin and the custom
                    // URL handler config are honored.
                    self.open_pane_link(ws, links_pane(ws), &uri);
                    return;
                }
                // As in Terminator, a left-click on a pane titlebar focuses the
                // pane, and a second click opens the EditPaneTitle overlay. The
                // two-click model (focus first, edit second) avoids accidental
                // title edits on focus transitions.
                //
                // The titlebar is also the grab handle for dragging the pane
                // elsewhere in the tab, so the press only FOCUSES and arms the
                // gesture; the release decides between editing the title and
                // moving the pane. Opening the editor on press would put a text
                // field over the pane the user just picked up.
                let (cx, cy) = (ws.cursor.x as f32, ws.cursor.y as f32);
                if bcode == 0
                    && let Some(clicked_pane_id) = self.pane_at_titlebar_click(ws, cx, cy)
                {
                    let already_focused = ws.mux.active_focus() == Some(clicked_pane_id);
                    let pre = self.focus_key(ws);
                    ws.mux.focus_at(area, cx, cy);
                    self.note_focus_change(ws, pre);
                    ws.pane_drag = Some(PaneDrag {
                        pane: clicked_pane_id,
                        origin: (cx, cy),
                        live: false,
                        rename_on_click: already_focused,
                        target: None,
                    });
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // A left-press on a split divider seam starts
                // a drag-to-resize gesture rather than focusing a pane / starting
                // a selection. Seams are hit-tested over the same content `area`
                // the panes lay out in, with a small grab tolerance.
                if self.press_split_divider(ws, bcode) {
                    return;
                }
                let pre = self.focus_key(ws);
                ws.mux
                    .focus_at(area, ws.cursor.x as f32, ws.cursor.y as f32);
                self.note_focus_change(ws, pre);
                if self.send_mouse(ws, bcode, true, false) {
                    return;
                }
                // Middle-click in the content area pastes the PRIMARY
                // selection when the platform exposes one; otherwise it falls
                // back to the regular clipboard.
                //
                // Mirrors Terminator's `disable_mouse_paste`
                // (terminatorlib/config.py:88): when true, middle-click does
                // not paste. Useful for terminal-of-last-resort use
                // cases where accidental middle-clicks shouldn't leak
                // clipboard content into commands.
                if bcode == 1 && !self.cfg.disable_mouse_paste {
                    self.paste_primary(ws);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Mirrors Terminator's `putty_paste_style`
                // (terminatorlib/config.py:89): right-click pastes (PuTTY/Windows
                // convention) instead of opening the context menu. The
                // companion `putty_paste_style_source_clipboard` decides
                // whether the source is CLIPBOARD or PRIMARY.
                if bcode == 2 && self.cfg.putty_paste_style {
                    match putty_paste_source(self.cfg.putty_paste_style_source_clipboard) {
                        PasteSource::Clipboard => self.paste_clipboard(ws),
                        PasteSource::Primary => self.paste_primary(ws),
                    }
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Click the scrollbar to jump the viewport, then drag it.
                // Keep this behind pane gestures and terminal mouse reporting;
                // the receipt excludes the same gutter in its own hit test.
                if bcode == 0
                    && let Some(grab) = self.scrollbar_jump(ws, area, px, py)
                {
                    ws.scrollbar_drag_offset = Some(grab);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Right-click handling, layered:
                //
                // 1. `Shift + right-click` *with* selected text, off it, keeps
                //    the extend-selection behavior (xterm convention; muscle
                //    memory for kettle's power users).
                // 2. Any other right-click opens the context menu at the
                //    click point (Terminator / GNOME / iTerm2 default),
                //    Shift+right-click on the selection or with nothing
                //    selected included, so its rows can act on them.
                //
                // Mouse-tracking already short-circuited above when the
                // focused program is consuming mouse events, so this only
                // fires for the kettle chrome; Shift bypasses tracking, so
                // Shift+right-click always reaches it.
                if bcode == 2 {
                    if ws.mods.shift_key()
                        && self.pointer_on_selection(ws, area) == Some(false)
                        && self.extend_selection_to_cursor(ws, area, bcode)
                    {
                        if self.cfg.copy_on_select {
                            self.copy_selection(ws);
                        }
                        if let Some(w) = &ws.window {
                            w.request_redraw();
                        }
                        return;
                    }
                    let offer_copied = ws.mods.shift_key();
                    self.open_context_menu(ws, px, py, offer_copied);
                    return;
                }
                if bcode == 0 {
                    self.begin_or_extend_mouse_selection(ws, area);
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button,
                ..
            } => {
                ws.resume_native_pointer();
                // Release report for the side buttons, so a tracking app sees
                // the matching button-up after the press. Only a press the
                // app saw gets one: a press that a context menu, a modal or
                // the media viewer took leaves no release behind, even when
                // that surface closed while the button was held.
                if let Some(sgr) = extra_mouse_sgr(button) {
                    ws.held_buttons.end_own(sgr);
                    if self.confirm_mouse_release(ws, sgr) {
                        return;
                    }
                    self.send_mouse(ws, sgr, false, false);
                    return;
                }
                let bcode = match button {
                    MouseButton::Left => 0,
                    MouseButton::Middle => 1,
                    MouseButton::Right => 2,
                    _ => return,
                };
                // A gesture of Kettle's own ends with this release, whichever
                // handler below takes it; a program's goes on to get its
                // release from `report_mouse`.
                ws.held_buttons.end_own(bcode);
                if self.confirm_mouse_release(ws, bcode) {
                    return;
                }
                // A release ends an editor drag inside the search bar; any
                // other release while the bar is open belongs to the grid
                // gesture it started (selection end, copy-on-select, mouse
                // reporting), like the ctl path.
                if bcode == 0
                    && search_pointer_route(ws.search.open, None, ws.search.pointer_captured())
                        == SearchPointerRoute::Bar
                {
                    ws.search.dragging_editor = false;
                    self.search_button_release(ws);
                    return;
                }
                // The release of a press a lane took is the lane's.
                if bcode == 0 && ws.lane_resize.is_some() {
                    self.end_lane_resize(ws);
                    self.sync_cursor_icon(ws);
                    return;
                }
                if bcode == 0
                    && let Some(drag) = ws.lane_drag.take()
                {
                    self.end_lane_drag(ws, drag);
                    self.sync_cursor_icon(ws);
                    return;
                }
                // The release of a press a card took is the card's too.
                let (px, py) = (ws.cursor.x as f32, ws.cursor.y as f32);
                if self.release_card(ws, px, py, bcode) {
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Terminator parity: the release is where a titlebar press
                // finally means something. Moved past the slop -> move the pane
                // to the latched drop target; never moved -> the
                // second-click-renames behaviour the press deferred.
                //
                // FIRST among the left-release handlers, ahead of the tear-off
                // and mouse-reporting branches that can return early. Those are
                // all mutually exclusive with a pane drag today -- the titlebar
                // press returns before any button is noted as held -- but "today"
                // is exactly the kind of reasoning that leaves a gesture armed
                // when one of them grows a new early return.
                if bcode == 0
                    && let Some(drag) = ws.pane_drag.take()
                {
                    if drag.live {
                        if let Some((target, _, dir, before)) = drag.target
                            && ws.mux.move_pane_beside(drag.pane, target, dir, before)
                        {
                            // Same two follow-ups every tree edit needs:
                            // re-size every PTY to its new rect, and persist the
                            // layout so a restore brings the pane back where the
                            // user put it.
                            self.resize_all(ws);
                            self.save_session(ws);
                        }
                    } else if drag.rename_on_click {
                        self.handle_action(ws, Action::EditPaneTitle, event_loop);
                    }
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                }
                // A left-release while a torn window is tracked is the DROP.
                // Two shapes: (a) the synthesized release winit posts to the
                // TORN window when the Windows modal move loop exits
                // (WM_EXITSIZEMOVE → WM_LBUTTONUP — verified in the vendored
                // 0.30.13 source); (b) the real release on the capture-holding
                // SOURCE in manual-follow. Commit the latched dock either way.
                // A left-release on an unrelated window while tracking is live
                // means the tracking went stale (an X11 drop we never
                // observed) — abandon it and process the release normally.
                //
                // Esc-cancel guard: WM_EXITSIZEMOVE
                // fires for EVERY modal-loop exit — Esc-cancel included —
                // and the synthesized release is indistinguishable from a
                // drop, while the latch survives the snap-back (the live
                // cursor is still over the band). The tell is PHYSICAL
                // button state: an Esc-cancel exits the loop with the
                // primary button still held; a real drop only exits after
                // it went up. Held ⇒ abandon (the user's real release later
                // arrives with tracking already cleared and flows through
                // normal processing harmlessly).
                if bcode == 0
                    && let Some(td) = self.torn_drag.as_ref()
                {
                    if td.seq == ws.seq || !td.native {
                        let commit = !primary_button_physically_held();
                        let commit = commit && (!td.caption || self.revalidate_dock_latch(ws));
                        self.finalize_torn_drag(ws, commit);
                        return;
                    }
                    self.finalize_torn_drag(ws, false);
                }
                // The release goes where its press did, if a program got it.
                if self.send_mouse(ws, bcode, false, false) {
                    return;
                }
                if selection_release_matches(ws.selection_button, bcode) {
                    self.finish_selection_gesture(ws);
                }
                if bcode == 0 {
                    ws.search.dragging_editor = false;
                    ws.scrollbar_drag_offset = None;
                    // End any split-divider drag on left-button up.
                    ws.dragging_split = None;
                    // End the drag-to-reorder gesture on
                    // left-button release. Any swaps that happened
                    // during the drag are already committed; this just
                    // disarms the CursorMoved handler.
                    ws.tab_drag_active = false;
                    ws.tab_drag_press = None;
                    ws.tab_pressed_idx = None;
                    // On Wayland only, a release while the detach FSM is
                    // OUTSIDE the window tears the dragged tab off into a new
                    // window. Everywhere else the tear fires the moment the
                    // cursor crosses the band threshold (`maybe_tear_off`), so
                    // the FSM is already Idle by release, and a release still
                    // inside the hysteresis slop deliberately does NOT tear
                    // (Chromium's within-slop = click/reorder semantics).
                    // Wayland can't position windows client-side nor hand off a
                    // drag to a surface that never saw the press, so it keeps
                    // the at-release tear.
                    let dropped_outside = matches!(
                        ws.detach_drag,
                        crate::detach::DragState::DraggingOutside { .. }
                    );
                    ws.detach_drag = std::mem::take(&mut ws.detach_drag).on_mouse_up();
                    ws.drag_press = None;
                    let wayland = ws.window.as_deref().is_some_and(window_is_wayland);
                    if self.cfg.detachable_tabs
                        && dropped_outside
                        && wayland
                        && ws.mux.tabs.len() > 1
                    {
                        let closing_idx = ws.mux.active;
                        self.release_held_buttons_in_tab(ws, closing_idx);
                        if let Some(dt) = ws.mux.detach_tab(closing_idx) {
                            // `outer_position` errs on Wayland — `None` lets
                            // the compositor place the window.
                            let pos = ws
                                .window
                                .as_ref()
                                .and_then(|w| w.outer_position().ok())
                                .map(|p| {
                                    winit::dpi::PhysicalPosition::new(
                                        p.x + ws.cursor.x as i32,
                                        p.y + ws.cursor.y as i32,
                                    )
                                    .into()
                                });
                            self.release_focus_report_to(ws, &dt);
                            let paste =
                                ws.take_moved_paste_state(&dt.tab, std::time::Instant::now());
                            match self
                                .open_window(
                                    event_loop,
                                    WindowOpen::AdoptTab {
                                        tab: dt,
                                        source_color: self.tear_off_source_color(ws),
                                        font_size: ws.renderer.as_ref().map(|r| r.font_size()),
                                        paste: Box::new(paste),
                                    },
                                    pos,
                                    None,
                                )
                                .map_err(|open| *open)
                            {
                                Ok(torn_seq) => {
                                    self.fire_tab_close_event(ws, closing_idx);
                                    // Agents see the tear-off too.
                                    self.ctl_broadcast(
                                        "tab_moved",
                                        None,
                                        serde_json::json!({
                                            "from_window": ws.seq,
                                            "to_window": torn_seq,
                                            "tab": closing_idx,
                                        }),
                                    );
                                    self.resize_all(ws);
                                    if let Some(w) = &ws.window {
                                        w.request_redraw();
                                    }
                                }
                                Err(WindowOpen::AdoptTab { tab: dt, paste, .. }) => {
                                    log::warn!(
                                        "tear-off: open_window failed; tab kept in source window"
                                    );
                                    ws.mux.attach_tab(dt, Some(closing_idx));
                                    ws.restore_moved_paste_state(*paste);
                                }
                                Err(_) => {
                                    unreachable!("open_window returns the WindowOpen it was given")
                                }
                            }
                        }
                    }
                }
            }
            // A pinch over a preview lane zooms its rendered item at the
            // pointer; anywhere else it does nothing.
            WindowEvent::PinchGesture { delta, .. } => {
                ws.resume_native_pointer();
                if self.lane_takes_wheel(ws) {
                    self.pinch_over_lane(ws, delta);
                }
            }
            WindowEvent::MouseWheel { delta, phase, .. } => {
                ws.resume_native_pointer();
                // A gesture ending (macOS momentum scroll, and any backend that
                // reports phases) drops leftover sub-detent residue so a partial
                // step can't leak into the next, unrelated gesture.
                if matches!(
                    phase,
                    winit::event::TouchPhase::Ended | winit::event::TouchPhase::Cancelled
                ) {
                    ws.wheel.reset();
                    ws.lane_wheel.reset();
                }
                if ws.confirm_dialog.is_some() {
                    ws.lane_wheel.reset();
                    let steps = ws.wheel.feed(&delta, self.cfg.scroll_multiplier);
                    self.confirm_wheel(ws, steps.lines);
                    return;
                }
                // A preview lane owns the wheel over it. Its motion is dropped
                // from the terminal's account rather than banked there, so no
                // part of it reaches the terminal once the lane closes; the
                // lane keeps its own, to scroll a source. Motion over the
                // terminal is dropped from the lane's the same way.
                if self.lane_takes_wheel(ws) {
                    ws.wheel.reset();
                    self.wheel_over_lane(ws, &delta);
                    return;
                }
                ws.lane_wheel.reset();
                let steps = ws.wheel.feed(&delta, self.cfg.scroll_multiplier);
                // Numbers only — never terminal content. Makes the
                // precision-touchpad class of bug diagnosable from a user's
                // `RUST_LOG=debug` run instead of requiring a custom build.
                log::debug!(
                    "wheel: delta={delta:?} phase={phase:?} -> notches={} lines={} cols={}",
                    steps.notches,
                    steps.lines,
                    steps.cols
                );
                if steps.is_zero() {
                    // Motion too small to move anything yet — it is banked in
                    // `ws.wheel`, not discarded.
                    return;
                }
                self.dispatch_wheel(ws, steps);
            }
            WindowEvent::DroppedFile(path) => {
                // A drop is the pointer's, as a press elsewhere: the keyboard
                // goes back to the terminal first.
                self.exit_preview_focus(ws);
                // A file dropped while a modal (search /
                // palette / settings / confirm dialog / inline title-edit / vi
                // copy-mode / …) is open must NOT inject its path into the PTY
                // behind the dialog — the same pointer-leak class
                // `modal_swallows_pointer` already closes for clicks. Unlike
                // that gate, an open context menu blocks the drop too.
                if self.any_modal_open(ws) {
                    return;
                }
                // Insert a shell-quoted path plus a trailing space. The shared
                // paste path preserves bracketed paste, per-pane shell
                // formatting, broadcast scope, and WSL translation.
                let paths = vec![path];
                let video = self.cfg.paste_video_preview.then(|| {
                    crate::video_preview::VideoPasteRequest::from_user_paths(
                        &paths,
                        crate::video_preview::VideoPasteSource::Drop,
                    )
                });
                self.paste_paths(ws, paths, true, video.flatten());
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Focused(f) => {
                ws.window_focused = f;
                ws.os_focus_known = true;
                if f {
                    self.update_toast_unavailable = None;
                    ws.frame_recovery.expedite(std::time::Instant::now());
                    // Another process may have installed or dismissed an
                    // update while this one was in the background.
                    self.request_update_facts(false);
                }
                // Track the OS focus in `focused_seq` so window-less events
                // (RemoteCommand, ctl "focused-window" ops, update notices)
                // and the agent-facing focused_window/to_window JSON
                // route to the window the user actually clicked — not whichever
                // window was last opened or ctl-focused. `ws.seq` is this
                // window's BTreeMap key; finish_window_dispatch re-inserts it
                // after dispatch.
                if f {
                    self.focused_seq = ws.seq;
                }
                if !f {
                    self.confirm_cancel_press(ws);
                    ws.confirm_pointer_release = false;
                    // An unfocused window draws no cursor, and the Metal layer
                    // beneath already shows the off phase.
                    Self::hide_cursor_layer(ws);
                    // Its release may never come here.
                    ws.lane_drag = None;
                    self.end_lane_resize(ws);
                    // The keyboard goes back to the terminal.
                    self.exit_preview_focus(ws);
                }
                // Non-interactive UI-state marker (OS-driven focus
                // change — a transition the PTY output stream can't show).
                if let Some(rec) = self.recorder.as_mut() {
                    rec.record_marker(if f {
                        "kettle:focus_in"
                    } else {
                        "kettle:focus_out"
                    });
                }
                // Broadcast Tab arms every target pane, including named-group
                // targets in other windows. Disarm the whole in-process fleet
                // together so a reply buffered before this focus boundary
                // cannot reopen when any sibling is focused later. The next
                // Tab explicitly arms a fresh generation.
                let completion_panes = ws.mux.panes.values().chain(
                    self.windows
                        .values()
                        .flat_map(|window| window.mux.panes.values()),
                );
                clear_completions_on_focus_loss(f, completion_panes, |pane| {
                    pane.term.invalidate_completion();
                });
                // A focus loss can swallow the button-UP that
                // ends an in-progress drag (the release lands on whatever window
                // took focus), latching `selecting` / `scrollbar_drag_offset` /
                // `tab_drag_active` / a held button. The next CursorMoved
                // then kept extending the selection or dragging a tab with no
                // button down. Disarm them on focus loss, committing any pending
                // copy-on-select first (mirrors the left-button-up path).
                if !f {
                    // The compositor may deliver held-key releases to the
                    // newly focused window. Drop UI suppression state here so
                    // a later unrelated release cannot be mistaken for the
                    // missing half of an old shortcut.
                    ws.suppressed_key_releases.clear();
                    ws.terminal_owned_key_releases.clear();
                    ws.terminal_key_recipients.clear();
                    ws.closing_keys.clear();
                    ws.ime_preedit = None;
                    ws.ime_focus_generation = ws.ime_focus_generation.wrapping_add(1);
                    ws.search.dragging_editor = false;
                    ws.search.pressed_control = None;
                    self.finish_selection_gesture(ws);
                    ws.scrollbar_drag_offset = None;
                    ws.scrollbar_hover = false;
                    ws.tab_drag_active = false;
                    ws.tab_drag_press = None;
                    ws.tab_pressed_idx = None;
                    // A focus loss also ends any split-divider drag.
                    ws.dragging_split = None;
                    // Releases now land in another window: each program
                    // with a held button's press gets its release here.
                    self.release_held_buttons(ws, |_| true);
                    // And a card's press: its release may land elsewhere.
                    ws.card_press = None;
                    // A focus loss also cancels an in-flight tab tear-off
                    // (the release will land on whatever window took focus).
                    ws.detach_drag = crate::detach::DragState::default();
                    ws.drag_press = None;
                    ws.pane_drag = None;
                    if let Some(receipt) = ws.media_paste_receipt.as_mut()
                        && receipt.set_hover(false, std::time::Instant::now())
                    {
                        ws.accessibility_pending = true;
                    }
                    // Torn-drag tracking deliberately survives
                    // this disarm — the tear itself moves OS focus to the
                    // torn window (firing Focused(false) on the source), and
                    // manual-follow rides the source's mouse CAPTURE, which
                    // outlives focus. Stale tracking is cleaned by the
                    // press/release handlers + the about_to_wait failsafe.
                }
                // Mirrors Terminator's `hide_on_lose_focus`
                // (terminatorlib/config.py:77): Quake-style auto-hide. When
                // the user clicks away to another window, hide the
                // kettle window. Reappears via `kettle --toggle`
                // or whatever global hotkey the user
                // bound. Honors only on focus-LOSS (f == false).
                if !f
                    && self.cfg.hide_on_lose_focus
                    && let Some(w) = &ws.window
                {
                    w.set_visible(false);
                }
                // Route through the shared helper so all
                // user-driven blink-reset paths share one implementation. The
                // CursorBlinkingChange handler still inlines the body
                // because it runs inside `ws.mux.panes.values_mut()`
                // and can't borrow `self` again — that one's documented.
                self.reset_blink_phase(ws);
                // Focus-event reporting (DEC private mode ?1004) follows
                // `window_focused` in `sync_pane_focus_reports`, which runs
                // when this dispatch finishes and also covers focus moving
                // between panes and tabs.
                // winit's `request_user_attention(None)` alone does
                // not reliably stop the Win11 taskbar flash once started, so
                // when an attention request is outstanding, clear it directly
                // (FlashWindowEx FLASHW_STOP via Taskbar) on focus-gain and
                // reset the tracker.
                if f && ws.attention_active {
                    ws.attention_active = false;
                    if let Some(w) = &ws.window {
                        w.request_user_attention(None);
                        ws.taskbar.clear_attention(w);
                    }
                }
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            WindowEvent::Occluded(occluded) => {
                // Suppress animation wakes while occluded; repaint on return.
                ws.window_occluded = occluded;
                if occluded {
                    Self::hide_cursor_layer(ws);
                }
                if !occluded {
                    ws.frame_recovery.expedite(std::time::Instant::now());
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                }
            }
            WindowEvent::Ime(ime) => {
                // Text through an input method returns focus from a card to
                // its pane, as a key press does; the method handles those
                // presses, so none arrives.
                let typing = match &ime {
                    winit::event::Ime::Preedit(text, _) => !text.is_empty(),
                    winit::event::Ime::Commit(_) => true,
                    _ => false,
                };
                if typing && ws.card_focus.take().is_some() {
                    ws.accessibility_pending = true;
                }
                // A preview lane holding the keyboard takes no text: what an
                // input method composes or commits meanwhile goes nowhere.
                if ws.preview_focus.is_some()
                    && matches!(
                        ime,
                        winit::event::Ime::Preedit(..) | winit::event::Ime::Commit(_)
                    )
                {
                    return;
                }
                let redraw = match ime {
                    winit::event::Ime::Enabled => {
                        if let Some(window) = &ws.window {
                            window.set_ime_allowed(ws.preview_focus.is_none());
                        }
                        self.update_ime_cursor_area(ws);
                        false
                    }
                    winit::event::Ime::Preedit(text, selection) => {
                        if !update_ime_preedit(ws, text, selection) {
                            return;
                        }
                        self.update_ime_cursor_area(ws);
                        true
                    }
                    winit::event::Ime::Commit(text) => {
                        let redraw = ws.ime_preedit.is_some() || !text.is_empty();
                        if take_ime_commit(ws) {
                            self.commit_ime_text(ws, &text, event_loop);
                        }
                        redraw
                    }
                    winit::event::Ime::Disabled => {
                        ws.ime_preedit_owner = None;
                        ws.ime_preedit.take().is_some()
                    }
                };
                if redraw && let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                // A key press returns focus from a card that assistive
                // technology focused to the pane, where the key goes.
                if event.state == ElementState::Pressed && ws.card_focus.take().is_some() {
                    ws.accessibility_pending = true;
                }
                let toast_owns_key = update_toast::owns_key(
                    self.update_toast_focus.is_some() && self.update_toast_projection(ws).is_some(),
                    &event.logical_key,
                    ws.mods,
                );
                let ui_owns_key = toast_owns_key
                    || !matches!(ws.detach_drag, crate::detach::DragState::Idle)
                    || ws.context_menu.is_some()
                    || ws.vi_mode.is_some()
                    || ws.hint_state.is_some()
                    || ws.palette_input.is_some()
                    || ws.settings_nav.is_some()
                    || ws.layout_picker_input.is_some()
                    || ws.theme_picker_input.is_some()
                    || ws.ssh_input.is_some()
                    || ws.confirm_dialog.is_some()
                    || ws.editing_title.is_some()
                    || ws.search.open
                    || ws.preview_focus.is_some()
                    || ws.ime_preedit.is_some();
                // The modal that gets this press, if any; a press after which
                // it is no longer the top modal closed or replaced it.
                let key_modal = top_modal(ws);
                if drop_closing_key_repeat(
                    &mut ws.closing_keys,
                    event.physical_key,
                    event.state,
                    event.repeat,
                ) {
                    return;
                }
                let release_consumed = track_consumed_key_release(
                    &mut ws.suppressed_key_releases,
                    &mut ws.terminal_owned_key_releases,
                    event.physical_key,
                    event.state,
                    ui_owns_key,
                );
                if event.state == ElementState::Released
                    && ws.confirm_dialog.is_some()
                    && confirmation_space_key(&event.logical_key)
                {
                    self.confirm_space_release(ws, release_consumed);
                    if release_consumed {
                        return;
                    }
                }
                if release_consumed {
                    return;
                }
                if event.state == ElementState::Released {
                    self.write_terminal_key_event(ws, &event, false);
                    return;
                }
                if !toast_owns_key && self.update_toast_focus.take().is_some() {
                    ws.accessibility_pending = true;
                    if let Some(window) = &ws.window {
                        window.request_redraw();
                    }
                }
                if toast_owns_key {
                    update_toast::guard_activation(
                        &event.logical_key,
                        event.physical_key,
                        &mut ws.closing_keys,
                    );
                    match event.logical_key {
                        Key::Named(NamedKey::Escape) => {
                            self.dismiss_update_toast(ws);
                            return;
                        }
                        Key::Named(NamedKey::Tab) => {
                            self.update_toast_focus = Some(
                                if self.update_toast_focus == Some(ACCESSIBILITY_UPDATE_DETAILS_ID)
                                {
                                    ACCESSIBILITY_UPDATE_DISMISS_ID
                                } else {
                                    ACCESSIBILITY_UPDATE_DETAILS_ID
                                },
                            );
                            ws.accessibility_pending = true;
                            if let Some(window) = &ws.window {
                                window.request_redraw();
                            }
                            return;
                        }
                        key if confirmation_space_key(&key)
                            || matches!(key, Key::Named(NamedKey::Enter)) =>
                        {
                            let details =
                                self.update_toast_focus == Some(ACCESSIBILITY_UPDATE_DETAILS_ID);
                            self.dismiss_update_toast(ws);
                            if details {
                                self.open_about_panel(ws);
                            }
                            return;
                        }
                        _ => {
                            self.update_toast_focus = None;
                            ws.accessibility_pending = true;
                        }
                    }
                }
                if ws.ime_preedit.is_some() {
                    return;
                }
                // Typing in the torn window right after an X11 drop whose
                // release the WM swallowed commits the latched dock first (same
                // shape as the CursorMoved post-drop heuristic, including the
                // post-handoff blackout and the final-position latch
                // revalidation; on Windows the synthesized release already
                // cleared the tracking before any key can arrive).
                if let Some(td) = self.torn_drag.as_ref()
                    && td.seq == ws.seq
                    && td.native
                    && td.saw_move
                    && td.started.elapsed() >= std::time::Duration::from_millis(300)
                {
                    let commit = self.revalidate_dock_latch(ws);
                    self.finalize_torn_drag(ws, commit);
                    // A committed merge empties this window — it is closing;
                    // don't process the key against an empty mux.
                    if self.window_close_pending(ws.seq) {
                        return;
                    }
                }
                let cursor_exit_key = if ws.cursor_exit_log.enabled {
                    ws.cursor_exit_log.accepted_key(
                        ws.seq,
                        ws.mux.tabs.get(ws.mux.active).map(|tab| tab.focus),
                        event.state,
                        &event.logical_key,
                        event.physical_key,
                        ws.blink_layer.is_active(),
                    )
                } else {
                    None
                };
                // Record before the remaining modal handlers consume the key.
                // Paste uses separate markers.
                {
                    let mods = ws.mods;
                    if let Some(rec) = self.recorder.as_mut() {
                        dev_record_key(rec, &event.logical_key, mods);
                    }
                }
                // Terminator parity: Esc abandons an in-flight pane drag, and
                // is consumed like the tab-drag cancel below it. Only a LIVE
                // drag claims the key — an armed-but-unmoved press is still
                // just a click, and swallowing Esc for it would eat the key
                // whenever the pointer happened to rest on a titlebar.
                //
                // Dropping the state is the whole cancel: nothing has been
                // committed yet. The move only happens at release, off
                // `pane_drag.target`, so a cleared gesture releases into a
                // no-op with the tree untouched.
                if ws.pane_drag.as_ref().is_some_and(|d| d.live)
                    && matches!(&event.logical_key, Key::Named(NamedKey::Escape))
                {
                    ws.pane_drag = None;
                    // Its repeats must not go on to close a modal or reach
                    // the terminal.
                    ws.closing_keys.insert(event.physical_key);
                    if let Some(w) = &ws.window {
                        if cursor_exit_key.is_some() {
                            ws.cursor_exit_log.request_exit(cursor_exit_key);
                        }
                        w.request_redraw();
                    }
                    return;
                }
                // Esc cancels an in-flight tab drag and is consumed, so it
                // never leaks to the PTY or closes a modal.
                if !matches!(ws.detach_drag, crate::detach::DragState::Idle)
                    && matches!(&event.logical_key, Key::Named(NamedKey::Escape))
                {
                    let (next, _restored) = std::mem::take(&mut ws.detach_drag).cancel();
                    ws.detach_drag = next;
                    ws.drag_press = None;
                    ws.tab_drag_active = false;
                    ws.tab_drag_press = None;
                    ws.tab_pressed_idx = None;
                    ws.closing_keys.insert(event.physical_key);
                    if let Some(w) = &ws.window {
                        if cursor_exit_key.is_some() {
                            ws.cursor_exit_log.request_exit(cursor_exit_key);
                        }
                        w.request_redraw();
                    }
                    return;
                }
                // Esc also cancels a MANUAL-FOLLOW torn drag (native cancels
                // belong to the WM's own move loop, whose grab-end the
                // button-state revalidation sorts out). Without it, the
                // release would commit whatever dock target happened to be
                // latched. Consumed, like the pre-tear cancel above; the torn
                // window stays where the drag left it.
                if self.torn_drag.as_ref().is_some_and(|t| !t.native)
                    && matches!(&event.logical_key, Key::Named(NamedKey::Escape))
                {
                    self.abandon_torn_drag(Some(ws));
                    ws.closing_keys.insert(event.physical_key);
                    return;
                }
                // Keep the cursor solid while actively typing.
                // Routes through the shared helper so the eight
                // user-driven blink-reset paths (Reset / focus changes /
                // modal close / typing / tab close / window focus /
                // DEC ?12 toggle) stay in lock-step.
                if cursor_exit_key.is_some() {
                    ws.cursor_exit_log.request_exit(cursor_exit_key);
                }
                self.reset_blink_phase(ws);
                // Hide the OS mouse cursor (configurable; default on, like
                // every modern terminal). Re-shown on the next CursorMoved.
                self.hide_mouse_cursor(ws);
                let text = event.text.as_ref().map(|s| s.as_str());

                // The confirmation modal owns keyboard input; non-navigation keys are consumed.
                // Tab/Shift+Tab and arrows move focus; Enter activates the focused button.
                // Esc closes the modal without dispatching.
                if ws.confirm_dialog.is_some() {
                    if !modal_repeat_is_inert(
                        ModalRepeat::ConfirmDialog,
                        event.repeat,
                        &event.logical_key,
                        ws.mods.control_key(),
                    ) {
                        self.confirm_dialog_key(ws, &event.logical_key, event_loop);
                    }
                    remember_closing_key(ws, key_modal, event.physical_key);
                    return;
                }
                if ws.context_menu.is_some() {
                    if !modal_repeat_is_inert(
                        ModalRepeat::ContextMenu {
                            vim_nav: self.cfg.vim_menu_nav,
                        },
                        event.repeat,
                        &event.logical_key,
                        ws.mods.control_key(),
                    ) {
                        self.context_menu_key(ws, &event.logical_key, text, event_loop);
                    }
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                // Vi-mode key dispatch. When
                // vi_mode is Some, intercept keys for vi-style
                // navigation before they reach the PTY. h/j/k/l move
                // the vi cursor; 0/$/g/G jump; Esc exits, and so does the
                // chord that entered it.
                if ws.vi_mode.is_some() {
                    if is_vi_mode_toggle_chord(
                        &self.cfg.keybinds,
                        &event.logical_key,
                        &event.physical_key,
                        ws.mods,
                    ) {
                        // A held chord leaves once, not on every repeat.
                        if !event.repeat {
                            self.exit_vi_mode(ws);
                        }
                    } else if !modal_repeat_is_inert(
                        ModalRepeat::ViMode,
                        event.repeat,
                        &event.logical_key,
                        ws.mods.control_key(),
                    ) {
                        self.vi_mode_key(ws, &event.logical_key, text);
                    }
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                if ws.hint_state.is_some() {
                    if !modal_repeat_is_inert(
                        ModalRepeat::Hint,
                        event.repeat,
                        &event.logical_key,
                        ws.mods.control_key(),
                    ) {
                        self.hint_key(ws, &event.logical_key, text);
                    }
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                if ws.palette_input.is_some() {
                    if !modal_repeat_is_inert(
                        ModalRepeat::Palette,
                        event.repeat,
                        &event.logical_key,
                        ws.mods.control_key(),
                    ) {
                        self.palette_key(ws, &event.logical_key, text, event_loop);
                    }
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                // The theme picker sits over Settings when the Theme row
                // opened it, so it takes the keys first.
                if ws.theme_picker_input.is_some() {
                    if !modal_repeat_is_inert(
                        ModalRepeat::Palette,
                        event.repeat,
                        &event.logical_key,
                        ws.mods.control_key(),
                    ) {
                        self.theme_picker_key(ws, &event.logical_key, text);
                    }
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                // While the inline path prompt is open it owns the keyboard
                // (typed text → the buffer; Enter/Esc finish it).
                if ws.settings_nav.is_some() && ws.settings_text_edit.is_some() {
                    self.settings_text_key(ws, &event.logical_key, text);
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }
                // Settings overlay key handling (exclusive modal).
                if ws.settings_nav.is_some() {
                    // Chord capture waits for a new press. Auto-repeat of the
                    // Space or Enter that started it would otherwise be
                    // captured, or refused with a notification, once per repeat.
                    let inert = if settings_capture_active(ws) {
                        event.repeat
                    } else {
                        // Only a repeat needs the focused row's kind.
                        let steps = event.repeat && self.settings_row_steps(ws);
                        modal_repeat_is_inert(
                            ModalRepeat::Settings { steps },
                            event.repeat,
                            &event.logical_key,
                            ws.mods.control_key(),
                        )
                    };
                    if !inert {
                        self.settings_key(ws, &event.logical_key, event_loop);
                    }
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                if ws.layout_picker_input.is_some() {
                    self.layout_picker_key(ws, &event.logical_key, text);
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                if ws.ssh_input.is_some() {
                    self.ssh_key(ws, &event.logical_key, text);
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                // Edit-title overlay key handler. Esc
                // cancels; Enter applies via apply_title_edit;
                // Backspace removes one char; printable text appends.
                if ws.editing_title.is_some() {
                    self.title_edit_key(ws, &event.logical_key, text);
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                if ws.search.open {
                    if event.repeat
                        && search_button_ignores_repeat(
                            ws.search.focused_control,
                            &event.logical_key,
                        )
                    {
                        return;
                    }
                    if !self.search_key(ws, &event.logical_key, Some(&event.physical_key), text) {
                        self.search_bar_shortcut(
                            ws,
                            &event.logical_key,
                            Some(&event.physical_key),
                            event.repeat,
                            event_loop,
                        );
                    }
                    remember_closing_key(ws, key_modal, event.physical_key);
                    if let Some(w) = &ws.window {
                        w.request_redraw();
                    }
                    return;
                }

                // A preview lane holding the keyboard takes every key, the
                // application's shortcuts included: Esc gives it back.
                if ws.preview_focus.is_some() {
                    self.preview_focus_key(ws, &event.logical_key, event.repeat);
                    remember_closing_key(ws, key_modal, event.physical_key);
                    return;
                }

                if let Some((trigger, act)) = resolve_keybind_action(
                    &self.cfg.keybinds,
                    Some(&event.logical_key),
                    Some(&event.physical_key),
                    ws.mods,
                ) && !self.chord_falls_through(ws, trigger, &act)
                {
                    track_consumed_key_release(
                        &mut ws.suppressed_key_releases,
                        &mut ws.terminal_owned_key_releases,
                        event.physical_key,
                        event.state,
                        true,
                    );
                    if !(event.repeat && toggles_once_per_press(&act)) {
                        self.handle_action(ws, act, event_loop);
                    }
                    return;
                }

                track_terminal_key_press(
                    &mut ws.suppressed_key_releases,
                    &mut ws.terminal_owned_key_releases,
                    event.physical_key,
                );
                self.write_terminal_key_event(ws, &event, true);
            }
            WindowEvent::RedrawRequested => self.redraw(ws),
            _ => {}
        }
    }

    /// Returns the absolute wake-up deadline this window wants
    /// (`None` = wait for events). The dispatch wrapper merges every window's
    /// request to the earliest deadline and sets the control flow once.
    fn about_to_wait_inner(
        &mut self,
        ws: &mut WindowState,
        event_loop: &ActiveEventLoop,
    ) -> Option<std::time::Instant> {
        if take_window_lifecycle_wakes(ws) {
            self.drain_events(ws);
        }
        #[cfg(windows)]
        if ws
            .mux
            .panes
            .values()
            .any(|pane| !pane.exit_observed && pane.term.direct_child_exit_at().is_some())
        {
            // Direct-child timestamps remain level-triggered through both
            // deadlines. This runs even for hidden windows and does not depend
            // on output or cursor animation to keep the policy clock moving.
            self.drain_events(ws);
        }
        self.process_pending_pane_restarts(ws);
        let held_child_reap_pending = ws.mux.poll_held_child_statuses();
        // A log worker can fail after PTY output becomes silent. Its direct
        // event-loop wake reaches this poll even when output-generation gating
        // correctly decides there is no frame to paint.
        self.sync_session_log_failures(ws);
        // Drain trailing recorder output before reap removes a
        // just-exited pane (covers the shell-exit → process-exit close path,
        // e.g. a fast `-e cmd` session).
        self.flush_recorder_output(ws);
        let reaped = ws.mux.reap();
        if reaped.layout_changed {
            ws.dragging_split = None;
        }
        self.fire_closed_pane_events(ws);
        // Binding, not an inline call on the right of `||`: see the matching
        // reap in `redraw`.
        let stale_title = clear_stale_title_edit(&ws.mux, &mut ws.editing_title);
        if reaped.layout_changed || stale_title {
            ws.pending_resize = true;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
        self.hoover_groups(ws);
        if reaped.mux_empty && ws.window.is_some() {
            self.save_session(ws);
            self.request_window_close(ws.seq);
            return None;
        }
        ws.search_queries
            .retain(|pane_id, _| ws.mux.panes.contains_key(pane_id));
        if ws
            .media_paste_receipt
            .as_ref()
            .is_some_and(|receipt| !ws.mux.panes.contains_key(&receipt.pane_id))
        {
            ws.media_paste_receipt = None;
            ws.accessibility_pending = true;
        }
        if ws
            .pending_video_paste_receipt
            .as_ref()
            .is_some_and(|receipt| !ws.mux.panes.contains_key(&receipt.pane_id))
        {
            ws.pending_video_paste_receipt = None;
        }
        if !self.cfg.paste_video_preview {
            ws.pending_video_paste_receipt = None;
        }
        let now = std::time::Instant::now();
        if let Some(Err(error)) = poll_screenshot_completion(ws) {
            fire_notify(
                self.ui_text.text(kettle_i18n::Text::PaletteTakeScreenshot),
                &error,
            );
        }
        let screenshot_wait = ws
            .screenshot_completion
            .as_ref()
            .map(|_| std::time::Duration::from_millis(50));
        if ws
            .pending_video_paste_receipt
            .as_ref()
            .is_some_and(|pending| pending.expired(now))
        {
            ws.pending_video_paste_receipt = None;
            ws.accessibility_pending = true;
            log::debug!("video paste receipt abandoned: preview worker response timed out");
        }
        let pending_video_receipt_wait = ws.pending_video_paste_receipt.as_ref().map(|pending| {
            pending
                .deadline()
                .saturating_duration_since(now)
                .max(std::time::Duration::from_millis(1))
        });
        #[cfg(windows)]
        let bounded_pty_exit_wait = next_bounded_pty_exit_wait(ws, now);
        // Catch the session changes no gesture announces — see `SESSION_SWEEP`.
        // Deliberately NOT part of the returned deadline: this must never be a
        // reason to wake up.
        if session_sweep_due(self.last_session_sweep, now, !ws.mux.tabs.is_empty()) {
            self.last_session_sweep = Some(now);
            self.save_session(ws);
        }
        let lua_wait = self.poll_pending_lua_commands(ws, event_loop, now);
        let remote_wait = if should_poll_remote_window(
            !self.pending_remote_commands.is_empty(),
            self.remote_spool_claim_pending,
            self.remote_command_retry.next_at.is_some(),
            ws.seq,
            self.focused_seq,
        ) {
            // An unclaimed spool belongs to the window focused when it is
            // finally claimed. Do not let the first BTreeMap window steal the
            // origin merely because a busy lock deferred the watcher event.
            self.poll_pending_remote_commands(ws, now)
        } else {
            None
        };
        let automation_wait = match (lua_wait, remote_wait) {
            (Some(lua), Some(remote)) => Some(lua.min(remote)),
            (Some(wait), None) | (None, Some(wait)) => Some(wait),
            (None, None) => None,
        };
        self.check_pending_run_deadlines(ws);
        let pending_run_wait =
            self.pending_runs
                .values()
                .map(|p| p.deadline)
                .min()
                .map(|soonest| {
                    soonest.saturating_duration_since(now).clamp(
                        std::time::Duration::from_millis(1),
                        std::time::Duration::from_millis(500),
                    )
                });
        let automation_wait = automation_wait
            .into_iter()
            .chain(screenshot_wait)
            .chain(pending_run_wait)
            .min();
        if self.window_close_pending(ws.seq) || self.quit_requested {
            return None;
        }
        // Once the GPU device is lost we no longer paint (see the redraw guard).
        // Drive renderer recovery from here instead of scheduling animation
        // wakes that would only re-enter the dead-device redraw path.
        if self.gpu.as_ref().is_some_and(|g| g.is_lost()) {
            // The global device-loss callback can wake the loop without a
            // window event. Quiesce pane output here before any recovery
            // backoff returns so a hot PTY cannot spin the dead render path.
            self.sync_output_wake_gate(ws);
            self.start_gpu_incident_if_needed();
            match self.gpu_recovery.poll(now, GPU_RECOVERY_SETTLE) {
                RecoveryAction::Wait(wait) => {
                    let next = wait.max(std::time::Duration::from_millis(1));
                    let next = automation_wait.map_or(next, |automation| automation.min(next));
                    let next = pending_video_receipt_wait.map_or(next, |pending| pending.min(next));
                    return Some(now + next);
                }
                RecoveryAction::Attempt { attempt_index } => {
                    let attempt = attempt_index.saturating_add(1);
                    let escalation = kettle_render::escalation_for_attempt(attempt_index);
                    let escalation_label = gpu_escalation_label(escalation);
                    if let Some(log) = self.gpu_incident.as_mut()
                        && let Err(error) = log.record_attempt(attempt, escalation_label)
                    {
                        log::warn!("GPU fault diagnostic write failed: {error}");
                    }
                    match self.try_recover_gpu(ws, event_loop, escalation, attempt) {
                        Ok(outcome) => {
                            if let Some(log) = self.gpu_incident.as_mut()
                                && let Err(error) = log.record_recovered(
                                    attempt,
                                    escalation_label,
                                    outcome.adapter,
                                    outcome.secondary_window_failures,
                                )
                            {
                                log::warn!("GPU fault diagnostic write failed: {error}");
                            }
                            self.gpu_incident = None;
                            self.gpu_incident_started_for_loss = false;
                            self.gpu_recovery.recovered();
                        }
                        Err(error) => {
                            if let Some(log) = self.gpu_incident.as_mut()
                                && let Err(write_error) =
                                    log.record_failure(attempt, escalation_label, &error)
                            {
                                log::warn!("GPU fault diagnostic write failed: {write_error}");
                            }
                            let backoff = gpu_recovery_backoff(attempt);
                            let now = self.gpu_recovery.failed(backoff);
                            let next = backoff.max(std::time::Duration::from_millis(1));
                            let next =
                                automation_wait.map_or(next, |automation| automation.min(next));
                            let next = pending_video_receipt_wait
                                .map_or(next, |pending| pending.min(next));
                            return Some(now + next);
                        }
                    }
                }
            }
        }
        if ws.pty_resize_retry.take_due(now) {
            if ws.renderer.is_some() {
                self.resize_all(ws);
            } else {
                // Preserve a bounded retry while a renderer rebuild temporarily
                // makes layout metrics unavailable.
                ws.pty_resize_retry.record_result(now, true);
            }
        }
        let pty_resize_retry_wait = ws
            .pty_resize_retry
            .remaining(now)
            .map(|wait| wait.max(std::time::Duration::from_millis(1)));
        let monitor_refresh_wait = if ws.output_refresh_probe_pending {
            match monitor_refresh_probe_wait(ws.output_refresh_probe_at, now, false) {
                Some(remaining) => Some(remaining),
                None => {
                    self.sync_output_frame_budget(ws, true);
                    None
                }
            }
        } else {
            None
        };
        let render_hidden = window_is_render_hidden(ws);
        self.sync_output_wake_gate(ws);
        let mut frame_recovery_wait = None;
        match ws.frame_recovery.poll(now, render_hidden) {
            FrameRecoveryPoll::Idle | FrameRecoveryPoll::Quiescent => {}
            FrameRecoveryPoll::Wait(wait) => {
                frame_recovery_wait = Some(wait.max(std::time::Duration::from_millis(1)));
            }
            FrameRecoveryPoll::Ready(FrameRecoveryAction::Redraw) => {
                if let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
            FrameRecoveryPoll::Ready(FrameRecoveryAction::RebuildRenderer) => {
                match self.try_recover_window_renderer(ws) {
                    Ok(()) => {
                        log::info!("window {} renderer/surface recovery succeeded", ws.seq);
                        if let Some(window) = &ws.window {
                            window.request_redraw();
                        }
                    }
                    Err(error) => {
                        log::warn!(
                            "window {} renderer/surface recovery failed: {error}",
                            ws.seq
                        );
                        if self.gpu.as_ref().is_some_and(|gpu| gpu.is_lost()) {
                            // The device callback owns escalation. Ensure the
                            // event loop turns once more even if no animation
                            // or PTY event is active.
                            frame_recovery_wait = Some(std::time::Duration::from_millis(1));
                        } else {
                            ws.frame_recovery.schedule_renderer_rebuild(now);
                            match ws.frame_recovery.poll(now, render_hidden) {
                                FrameRecoveryPoll::Wait(wait) => {
                                    frame_recovery_wait =
                                        Some(wait.max(std::time::Duration::from_millis(1)));
                                }
                                FrameRecoveryPoll::Quiescent => {}
                                FrameRecoveryPoll::Idle | FrameRecoveryPoll::Ready(_) => {
                                    // A failed rebuild after the first
                                    // immediate attempt always arms a positive
                                    // backoff. Keep a defensive one-turn wake
                                    // if that invariant changes.
                                    frame_recovery_wait = Some(std::time::Duration::from_millis(1));
                                }
                            }
                        }
                    }
                }
            }
        }
        // Most backends pair ScaleFactorChanged with Resized. If one does not,
        // commit the pending DPI layout from the window's final inner size
        // here. A minimized 0x0 window and any renderer/GPU repair keep the
        // transition pending until a usable surface exists.
        let mut dpi_fallback_wait = None;
        if let Some((width, height)) = ws
            .window
            .as_ref()
            .map(|window| window.inner_size())
            .map(|size| (size.width, size.height))
        {
            let renderer_ready = self.dpi_resize_renderer_ready(ws)
                && !render_hidden
                && !ws.dpi_resize_surface_suspended;
            match ws.dpi_resize.on_event(DpiResizeEvent::AboutToWait {
                width,
                height,
                renderer_ready,
            }) {
                DpiResizeAction::ApplyLayout => self.commit_window_resize(ws, width, height),
                DpiResizeAction::DeferAndWake => {
                    // Do not render a transient new-scale/old-size frame just
                    // to turn the loop. A precise short deadline gives the
                    // backend's paired Resized event one more dispatch turn.
                    dpi_fallback_wait = Some(std::time::Duration::from_millis(1));
                }
                DpiResizeAction::Defer => {}
            }
        }
        let (completion_hide_due, completion_hide_wait) = ws
            .mux
            .active_focus()
            .and_then(|pane_id| ws.mux.panes.get(&pane_id))
            .map(|pane| pane.term.poll_completion_hide(now))
            .unwrap_or((false, None));
        // Drive cursor blink + visual-bell decay without busy-looping: only
        // schedule wake-ups while something is actually animating. A flash
        // that has fully decayed is dropped here and gets one more repaint to
        // erase its last frame, like the resize chip below.
        let bells_expired = ws
            .bell_flashes
            .values()
            .any(|rang| rang.elapsed() >= BELL_FLASH_DURATION);
        if bells_expired {
            ws.bell_flashes
                .retain(|_, rang| rang.elapsed() < BELL_FLASH_DURATION);
            if !render_hidden && let Some(w) = &ws.window {
                w.request_redraw();
            }
        }
        let bell_active = !render_hidden && !ws.bell_flashes.is_empty();
        // The resize chip needs repaints until it expires (then one more to
        // erase it); clear the state once it has.
        let update_toast_wait = self.tick_update_toast(ws, now);
        let resize_chip_live = ws
            .resize_overlay
            .map(|(_, _, t)| t.elapsed() < RESIZE_OVERLAY_DURATION)
            .unwrap_or(false);
        let resize_chip_active = resize_chip_live && !render_hidden;
        if !resize_chip_live && ws.resize_overlay.is_some() {
            ws.resize_overlay = None;
            if !render_hidden && let Some(w) = &ws.window {
                w.request_redraw();
            }
        }
        let receipt_enabled = ws
            .media_paste_receipt
            .as_ref()
            .is_some_and(|receipt| match &receipt.kind {
                crate::window_state::MediaPasteReceiptKind::Image { .. } => {
                    self.cfg.paste_image_preview
                }
                crate::window_state::MediaPasteReceiptKind::Video { .. } => {
                    self.cfg.paste_video_preview
                }
            });
        let receipt_owner_visible = receipt_enabled
            && ws.window_focused
            && terminal_surface_available(ws)
            && ws
                .media_paste_receipt
                .as_ref()
                .is_some_and(|receipt| ws.mux.active_focus() == Some(receipt.pane_id));
        let mut media_receipt_redraw = false;
        if !receipt_enabled && ws.media_paste_receipt.take().is_some() {
            // A live config change to `off` is an ownership boundary, not only
            // a paint toggle. Release the bounded pixels and retained path
            // immediately instead of keeping hidden preview state until its
            // original lifetime expires.
            ws.accessibility_pending = true;
            media_receipt_redraw = true;
        }
        if !receipt_owner_visible
            && let Some(receipt) = ws.media_paste_receipt.as_mut()
            && receipt.set_hover(false, now)
        {
            ws.accessibility_pending = true;
            media_receipt_redraw = true;
        }
        if ws
            .media_paste_receipt
            .as_ref()
            .is_some_and(|receipt| !receipt.live(now))
        {
            ws.media_paste_receipt = None;
            ws.accessibility_pending = true;
            media_receipt_redraw = true;
        } else if let Some(receipt) = ws.media_paste_receipt.as_mut() {
            media_receipt_redraw |= receipt.mark_collapsed_if_due(now);
        }
        // Cards that moved under a still pointer, or one under it settling,
        // change the cursor and the hover outline without a pointer event.
        if ws.card_hover_stale || ws.card_settle_wake.is_some_and(|at| at <= now) {
            ws.card_hover_stale = false;
            self.sync_cursor_icon(ws);
        }
        // The tip leaves its card when its time is up.
        if self.cards_tip.until().is_some_and(|until| until <= now) {
            self.end_cards_tip(ws);
        }
        // A lane that can no longer use the keyboard gives it back.
        self.check_preview_focus(ws);
        // Zoomed lanes whose view settled ask for sharper pixels, and a
        // lane's edge drag that paused resizes its PTYs.
        let lane_crop_wait = self.ask_due_lane_crops(ws, now);
        let lane_resize_wait = self.flush_lane_resize(ws, now);
        // Lane previews step their frames, time out, or start on a rest.
        let lane_preview_wait = self.tick_lane_previews(ws, now);
        // Lane playbacks keep their sessions, and pause while hidden.
        let lane_playback_wait = self.tick_lane_playbacks(ws, now);
        // A menu or dialog opened from the keyboard ends a lane's drag now,
        // not at the pointer's next motion.
        if (ws.lane_drag.is_some() || ws.lane_resize.is_some())
            && (ws.context_menu.is_some() || self.non_search_modal_open(ws))
        {
            ws.lane_drag = None;
            self.end_lane_resize(ws);
        }
        let cards_tip_wait = self.cards_tip.until().map(|until| {
            until
                .saturating_duration_since(now)
                .max(std::time::Duration::from_millis(1))
        });
        let card_settle_wait = ws.card_settle_wake.map(|at| {
            at.saturating_duration_since(now)
                .max(std::time::Duration::from_millis(1))
        });
        let media_receipt_wait = ws
            .media_paste_receipt
            .as_ref()
            .and_then(|receipt| receipt.next_deadline(now))
            .map(|deadline| {
                deadline
                    .saturating_duration_since(now)
                    .max(std::time::Duration::from_millis(1))
            });
        // `about_to_wait` sits on the same input-to-present path as a queued
        // menu-hover redraw on winit backends that deliver RedrawRequested in
        // the next turn. Reuse the validated snapshot's DEC blink bit while a
        // context menu is open; otherwise this scheduler quietly reacquires the
        // focused Term mutex even though `redraw` itself is lock-free.
        let cached_menu_snapshots = ws.context_menu.is_some()
            && context_menu_snapshot_reuse_safe(ws)
            && self.torn_drag.is_none()
            && self.pane_snapshots_match_current_layout(ws);
        let pane_blink = self.pane_cursor_blinking(ws, cached_menu_snapshots);
        let blink_timed_out = cursor_blink_timed_out(
            now.saturating_duration_since(ws.last_blink_activity),
            self.cfg.cursor_blink_timeout(),
            ws.blink_on,
        );
        // While the window server blinks the cursor, the scheduler neither
        // flips the phase nor wakes for it; `redraw` materializes the phase
        // from the layer's plan instead.
        let layer_blink = ws.blink_layer.is_active();
        let blink_active = !render_hidden
            && !layer_blink
            && !blink_timed_out
            && cursor_blink_active(self.cfg.cursor_blink, pane_blink, ws.window_focused);
        let blink_interval = std::time::Duration::from_millis(self.cfg.cursor_blink_interval);
        let blink_elapsed = now.saturating_duration_since(ws.last_blink);
        let next_blink =
            next_cursor_blink_phase(blink_active, blink_elapsed, blink_interval, ws.blink_on);
        let blink_due = next_blink.is_some();
        if let Some(next_blink) = next_blink {
            // Advance at the TIMER edge, before request_redraw(). Wayland may
            // defer RedrawRequested until a compositor frame callback; leaving
            // the old timestamp in place during that gap repeatedly enqueues
            // the same blink and drains the queue at full frame rate.
            ws.blink_on = next_blink;
            ws.last_blink = now;
        }
        // Whether only the blink drew in the half-period this edge ends.
        let blink_quiet = blink_due
            && ws
                .blink_layer
                .note_edge(now, crate::cursor_blink::quiet_window(blink_interval));
        let term_anim = !render_hidden
            && ws
                .mux
                .panes
                .values()
                .any(|p| p.term.has_running_animation());
        let term_anim_finished = std::mem::replace(&mut ws.term_animation_active, term_anim)
            && !term_anim
            && !render_hidden;
        // Wake at background frame boundaries, subject to the focus policy.
        // Output-driven repaints may still advance a focus-paused background.
        let bg_anim_interval_raw = ws
            .renderer
            .as_ref()
            .and_then(|r| r.bg_anim_interval_ms(&self.cfg, ws.window_focused));
        // Hidden windows add no background-animation wakes.
        let bg_hidden = bg_anim_interval_raw.is_some() && render_hidden;
        let bg_anim_interval = if bg_hidden {
            None
        } else {
            bg_anim_interval_raw
        };
        // Edge-trigger the bg redraw. Request it ONLY when the displayed frame
        // index changes, since requesting it on every `about_to_wait` makes
        // winit redraw continuously. This mirrors how `blink_due` gates the
        // cursor-blink redraw.
        let bg_frame = if bg_hidden {
            None
        } else {
            ws.renderer
                .as_ref()
                .and_then(|r| r.bg_current_frame_index(&self.cfg, ws.window_focused))
        };
        let bg_frame_due = bg_frame.is_some() && bg_frame != ws.last_bg_frame;
        if bg_frame_due {
            ws.last_bg_frame = bg_frame;
        }
        if bg_frame.is_none() {
            ws.last_bg_frame = None;
        }
        // Selection-autoscroll runs at the same ~30 fps as bell / image
        // animation. Without an active wake-up the edge drag freezes until
        // the user wiggles the mouse.
        let autoscroll_active = !render_hidden && ws.selecting && {
            let area = self.area(ws);
            let scale = ws
                .window
                .as_ref()
                .map_or(1.0, |window| window.scale_factor() as f32);
            ws.selecting_pane
                .and_then(|id| self.pane_terminal(ws, area, id))
                .map(|r| {
                    ws.selection_autoscroll_edge != 0
                        || selection_autoscroll_lines(
                            ws.cursor.y as f32,
                            r.1,
                            r.1 + r.3,
                            scale,
                            ws.selection_dragged,
                        ) != 0
                })
                .unwrap_or(false)
        };
        // A deferred (coalesced) output paint becomes due
        // `OUTPUT_FRAME_BUDGET` after the last frame. Until then it stays
        // pending and we wake at its deadline so the burst paints exactly once.
        let output_budget = effective_output_budget(ws.output_frame_budget, ws.flood_paints);
        let coalesce_due = !render_hidden
            && !ws.frame_recovery.has_pending()
            && ws.output_pacer.is_deferred()
            && !ws.output_pacer.redraw_queued()
            && ws
                .last_paint
                .map(|t| now.saturating_duration_since(t) >= output_budget)
                .unwrap_or(true);
        // At an edge that hides the cursor, a quiet window with nothing else
        // to draw hands its blink to the window server (macOS). The blink
        // redraw requested below becomes the hand-off frame.
        let cursor_eligible = crate::macos_cursor_layer::SUPPORTED
            && self.cfg.macos_cursor_blink_layer
            && next_blink == Some(false)
            && Self::pane_cursor_layer_eligible(ws);
        if crate::cursor_blink::handoff_allowed(crate::cursor_blink::HandoffInputs {
            enabled: crate::macos_cursor_layer::SUPPORTED
                && self.cfg.macos_cursor_blink_layer
                && ws.blink_layer.fallback().is_none(),
            edge_hides_cursor: next_blink == Some(false),
            blink_active,
            cursor_eligible,
            quiet: blink_quiet,
            other_animation: bell_active
                || term_anim
                || bg_anim_interval.is_some()
                || autoscroll_active
                || resize_chip_active
                || media_receipt_redraw
                || media_receipt_wait.is_some()
                || pending_video_receipt_wait.is_some()
                || completion_hide_due
                || completion_hide_wait.is_some(),
            ime_preedit: ws.ime_preedit.is_some(),
            modal_steady: modal_holds_cursor_steady(ws),
            work_pending: coalesce_due
                || ws.output_pacer.is_deferred()
                || ws.frame_recovery.has_pending()
                || ws.pending_resize
                || ws.accessibility_pending
                || dpi_fallback_wait.is_some()
                || pty_resize_retry_wait.is_some()
                || ws
                    .renderer
                    .as_ref()
                    .is_some_and(Renderer::has_pending_screenshot),
        }) {
            ws.blink_layer.request_handoff();
        }
        // A theme schedule flips at a wall-clock boundary, and only a redraw
        // applies it. A window that draws nothing (idle, or with its cursor
        // blinking on the window server's layer) would otherwise keep the old
        // theme until something else changes, so check the schedule at each
        // wall-clock minute and draw only when it has flipped.
        let theme_schedule_due = !render_hidden
            && theme_schedule_redraw_due(
                self.last_schedule_decision,
                self.theme_schedule_is_dark(),
            );
        let theme_schedule_wait =
            (!render_hidden && self.cfg.theme_schedule.is_some()).then(until_next_wall_minute);
        if (bell_active
            || blink_due
            || term_anim
            || term_anim_finished
            || bg_frame_due
            || autoscroll_active
            || coalesce_due
            || resize_chip_active
            || media_receipt_redraw
            || completion_hide_due
            || theme_schedule_due)
            && let Some(w) = &ws.window
        {
            w.request_redraw();
            if coalesce_due {
                ws.output_pacer.queue_deferred_redraw();
            }
        }
        // Pick the earliest wake we still need: ~30 fps for bell / animation /
        // autoscroll / the resize chip, the cursor-blink half-period deadline,
        // or the pending coalesced output paint's deadline.
        let blink_wait = blink_active.then(|| {
            let remaining = blink_interval.saturating_sub(blink_elapsed);
            remaining.max(std::time::Duration::from_millis(1))
        });
        let mut wait = animation_wait(
            bell_active || term_anim || autoscroll_active || resize_chip_active,
            bg_anim_interval,
            blink_wait,
        );
        if let Some(next) = frame_recovery_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = theme_schedule_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = dpi_fallback_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = pty_resize_retry_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = monitor_refresh_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = automation_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = completion_hide_wait {
            let next = next.max(std::time::Duration::from_millis(1));
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = update_toast_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = media_receipt_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = card_settle_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = cards_tip_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = lane_crop_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = lane_resize_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = lane_preview_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = lane_playback_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if let Some(next) = pending_video_receipt_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        #[cfg(windows)]
        if let Some(next) = bounded_pty_exit_wait {
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if held_child_reap_pending {
            let next = std::time::Duration::from_secs(1);
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        if ws.output_pacer.is_deferred()
            && !ws.output_pacer.redraw_queued()
            && !render_hidden
            && !ws.frame_recovery.has_pending()
        {
            let remaining = ws
                .last_paint
                .map(|t| output_budget.saturating_sub(now.saturating_duration_since(t)))
                .unwrap_or_default();
            let next = remaining.max(std::time::Duration::from_nanos(1));
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        // Search's deferred unlimited pass is deadline-driven and chunked.
        // Wake exactly when the 500 ms typing debounce expires; while a chunk
        // job remains, schedule the next event-loop turn without polling at a
        // frame-rate forever (the job clears at a match or terminal edge).
        if ws.search.open {
            // Repaint once when the bar should start saying `Searching…`.
            if let Some(at) = ws.search.searching_label_at() {
                if now < at {
                    ws.search.searching_label_wake = Some(at);
                    let next = at.saturating_duration_since(now);
                    wait = Some(wait.map_or(next, |current| current.min(next)));
                } else if ws.search.searching_label_wake.take().is_some()
                    && let Some(window) = &ws.window
                {
                    window.request_redraw();
                }
            }
            if let Some(deadline) = ws.search.unlimited_retry_at {
                let due = now >= deadline;
                let next = if due {
                    std::time::Duration::from_millis(1)
                } else {
                    deadline
                        .saturating_duration_since(now)
                        .max(std::time::Duration::from_millis(1))
                };
                wait = Some(wait.map_or(next, |current| current.min(next)));
                if due && let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
            if ws.search.background.is_some() {
                let next = std::time::Duration::from_millis(1);
                wait = Some(wait.map_or(next, |current| current.min(next)));
                if let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
        }
        if ws.accessibility_pending {
            let remaining =
                ws.accessibility_updated_at
                    .map_or(std::time::Duration::ZERO, |updated| {
                        ACCESSIBILITY_UPDATE_INTERVAL
                            .saturating_sub(now.saturating_duration_since(updated))
                    });
            let due = remaining.is_zero();
            let next = if due {
                std::time::Duration::from_millis(1)
            } else {
                remaining.max(std::time::Duration::from_millis(1))
            };
            wait = Some(wait.map_or(next, |current| current.min(next)));
            if due && let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
        // `show` pushes waiting past their deadline are answered, and those
        // whose pane closed are dropped, even when nothing else wakes us.
        self.media_tick(ws);
        self.prune_previews(ws);
        self.open_ready_previews(ws);
        if let Some(soonest) = self.media.next_deadline() {
            let next = soonest.saturating_duration_since(now).clamp(
                std::time::Duration::from_millis(1),
                std::time::Duration::from_millis(500),
            );
            wait = Some(wait.map_or(next, |current| current.min(next)));
        }
        // A worker failure may already be visible before this callback begins,
        // so comparing status only within this turn would miss the edge. The
        // notifier is edge-triggered; this unconditional state sync is cheap.
        if self.recorder.is_some() {
            let body = self
                .ui_text
                .text(kettle_i18n::Text::NotifyBodyRecordingFlushFailed);
            self.sync_recording_state(ws, body);
        }
        wait.map(|remaining| now + remaining)
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(recorder) = self.recorder.as_mut() {
            // Fatal renderer or event-loop exits can bypass CloseRequested.
            // Marking shutdown asynchronous here keeps their cleanup path from
            // inheriting arbitrary recording-device latency.
            recorder.begin_finish();
        }
    }
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

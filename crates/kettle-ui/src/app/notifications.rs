use super::*;

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

pub(super) fn input_rejection_message(result: PaneInputResult) -> Option<&'static str> {
    match result {
        PaneInputResult::Queued | PaneInputResult::ReadOnly => None,
        PaneInputResult::Backpressured => Some(
            "Input could not be delivered because a child is not accepting it fast enough. If broadcast mode is active, some panes may already have received it; verify before retrying.",
        ),
        PaneInputResult::Oversize => Some(
            "Input exceeded Kettle's per-message safety limit and was not delivered. If broadcast mode is active, verify the other panes before retrying.",
        ),
        PaneInputResult::PathNotTyped => {
            Some("A file name cannot be typed exactly into the target shell; pasted nothing.")
        }
        PaneInputResult::Failed => {
            Some("The pane's input transport failed. Kettle will close the failed pane.")
        }
    }
}

/// The notification for a rejected input, in the UI's language. Logs keep
/// [`input_rejection_message`]'s English.
pub(super) fn input_rejection_text(
    result: PaneInputResult,
    tr: &kettle_i18n::Translator,
) -> Option<&'static str> {
    use kettle_i18n::Text;
    input_rejection_message(result)?;
    Some(tr.text(match result {
        PaneInputResult::Backpressured => Text::NotifyBodyInputBackpressured,
        PaneInputResult::Oversize => Text::NotifyBodyInputOversize,
        PaneInputResult::PathNotTyped => Text::NotifyBodyPathNotTyped,
        _ => Text::NotifyBodyInputFailed,
    }))
}

pub(super) fn should_notify_input_rejection(
    last: Option<(PaneInputResult, std::time::Instant)>,
    result: PaneInputResult,
    now: std::time::Instant,
) -> bool {
    input_rejection_message(result).is_some()
        && last.is_none_or(|(prior, at)| {
            prior != result || now.saturating_duration_since(at) >= INPUT_REJECTION_NOTICE_INTERVAL
        })
}

/// Queue a desktop notification without ever waiting on an OS notification
/// service from Kettle's UI thread. The single bounded worker preserves order;
/// a stalled backend fills the queue and drops later notifications instead of
/// freezing rendering, input, and control replies.
pub(crate) fn fire_notify(title: &str, body: &str) {
    crate::queue_desktop_notification(title, body);
}

impl App {
    pub(super) fn report_input_result(&mut self, result: PaneInputResult) {
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

    /// Reflect the FOCUSED pane's OSC 9;4 progress onto the OS
    /// taskbar button each frame (pwsh 7 / Windows Terminal parity). Reads the
    /// focused pane the same way the cursor-blink poll does; `Taskbar` dedups
    /// internally, so an unchanged value costs nothing. No-op off Windows.
    pub(super) fn poll_taskbar_progress(&mut self, ws: &mut WindowState) {
        let progress = ws
            .mux
            .active_focus()
            .and_then(|id| ws.mux.panes.get(&id))
            .and_then(|p| p.term.progress());
        if let Some(window) = ws.window.clone() {
            ws.taskbar.apply(&window, progress);
        }
    }
}

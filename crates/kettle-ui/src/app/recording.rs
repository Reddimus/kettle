use super::*;

/// Terminator parity (`plugins/logger.py`): build the
/// per-pane session-log path. Lives under `<cache>/kettle/logs/`
/// (XDG-respecting via env probe; falls back to `./kettle-logs/`
/// when no cache dir is available).
///
/// File name shape: `kettle-<unix-secs>-<pid>-<sequence>.log`. The process-wide
/// sequence distinguishes panes and repeated logging sessions in one second.
pub(super) fn session_log_path(
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

/// Record named keys and modifiers; redact printable text unless raw input is enabled.
/// Keystroke counts and timing remain visible.
pub(super) fn dev_record_key(
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

pub(super) const RECORDING_TITLE_SUFFIX: &str = " [REC]";
// A distinct suffix while RAW keystroke capture is active, so literal-keystroke
// (potentially password-capturing) recording is never visually silent.
pub(super) const RECORDING_RAW_TITLE_SUFFIX: &str = " [REC RAW]";
pub(super) const RECORDING_LIMIT_TITLE_SUFFIX: &str = " [REC LIMIT]";
pub(super) const RECORDING_OVERLOAD_TITLE_SUFFIX: &str = " [REC INCOMPLETE]";
pub(super) const RECORDING_ERROR_TITLE_SUFFIX: &str = " [REC ERROR]";

pub(super) fn recording_window_title(
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

pub(super) fn effective_record_status(
    recorder: Option<crate::dev_record::RecordStatus>,
    start_failed: bool,
) -> Option<crate::dev_record::RecordStatus> {
    recorder.or(start_failed.then_some(crate::dev_record::RecordStatus::IoError))
}

pub(super) fn activation_recording_available(
    required: bool,
    status: Option<crate::dev_record::RecordStatus>,
) -> bool {
    !required || status == Some(crate::dev_record::RecordStatus::Recording)
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

impl App {
    pub(super) fn sync_session_log_failures(&self, ws: &WindowState) {
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

    /// Fan out PTY output still queued in
    /// the shared output sidechannels right now. Output that lands after the
    /// last redraw drain and before a pane is reaped or its window closes would
    /// otherwise be lost to both the recorder and Lua hooks. Call this before
    /// `mux.reap()` and on close so both consumers keep their tail. Recorder
    /// events batch on the bounded persistence worker; clean close paths hand
    /// finalization to that worker.
    pub(super) fn flush_recorder_output(&mut self, ws: &mut WindowState) {
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
    pub(super) fn drain_recorder_output_once(&mut self, ws: &mut WindowState) -> bool {
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

    pub(super) fn recording_status(&self) -> Option<crate::dev_record::RecordStatus> {
        effective_record_status(
            self.recorder.as_ref().map(|rec| rec.status()),
            self.recording_start_failed,
        )
    }

    /// Reflect terminal recorder failures in both persistent window state and
    /// an edge-triggered desktop notification during the same event-loop turn.
    pub(super) fn sync_recording_state(&mut self, ws: &mut WindowState, io_error_body: &str) {
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
}

//! What runs when the event loop ends, however it ends.
//!
//! There are two ways out. Closing the last window ends the loop and
//! `run_app` returns. Quitting through AppKit's `terminate:` (Cmd+Q, the
//! Dock's or the app menu's Quit, logout, shutdown) does not: winit calls
//! `ApplicationHandler::exiting` and AppKit then calls `exit()`, so nothing
//! after `run_app` runs and no destructor does either. Everything that must
//! happen before the process goes therefore happens from `exiting`, and again
//! after `run_app` for the paths that never reach it, guarded so it runs once.

use super::*;

/// What the exit teardown releases, borrowed from the App so the teardown can
/// be tested without building one.
pub(super) struct ExitParts<'a> {
    /// Set once the teardown has run; it never runs twice.
    pub(super) done: &'a mut bool,
    pub(super) recorder: Option<&'a mut crate::dev_record::Recorder>,
    pub(super) png_encoder: &'a mut Option<crate::paste_image::PngEncoder>,
    pub(super) pasted_images: &'a mut crate::paste_image::PastedImages,
    pub(super) viewer_copies: &'a std::sync::Mutex<crate::paste_image::PastedImages>,
    pub(super) document_copies: &'a crate::media::PrivateCopies,
    pub(super) video_copies: &'a crate::media::PrivateCopies,
    pub(super) session_writer: &'a mut crate::session::SessionWriter,
}

/// Release what must not outlive the process, the first time only, and say
/// whether this call did it.
///
/// Its cost is bounded by what it already was after `run_app`: deleting the
/// private copies, joining the session writer (at most the one write in
/// progress and the latest pending one) and `flush_notifications`, which the
/// App bounds at 250 ms. The recorder is only handed its finish, as
/// `close_window_now` does, so a stalled recording device cannot hold the
/// quit up.
pub(super) fn run_exit_teardown(parts: ExitParts<'_>, flush_notifications: impl FnOnce()) -> bool {
    if std::mem::replace(parts.done, true) {
        return false;
    }
    if let Some(recorder) = parts.recorder {
        recorder.begin_finish();
    }
    // Pasted screenshots are captured screen content, so they do not outlive
    // the session that produced them. Runs on the error path too — an exit
    // caused by a failure is exactly when leftovers would go unnoticed.
    // PNGs still being encoded are stopped and kept first, so cleanup
    // takes them too.
    if let Some(encoder) = parts.png_encoder.take() {
        for encoded in encoder.close() {
            let _ = parts.pasted_images.keep_png(encoded);
        }
    }
    parts.pasted_images.cleanup();
    // So are the copies handed to the image viewer, and the store closes
    // so an open still on its way cannot leave a new one.
    parts
        .viewer_copies
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .close();
    // And the copies handed to the document viewer and the video
    // player; a copy on its way stops first.
    parts.document_copies.close();
    parts.video_copies.close();
    // After the deletions, so they overlap the write `exiting` just handed
    // the writer.
    parts.session_writer.finish();
    flush_notifications();
    true
}

/// The window `save_session_on_exit` saves through, from each window's
/// sequence number and whether it has tabs: the first that has tabs and is
/// not the quick terminal (`quick`). `None` saves nothing.
pub(super) fn exit_save_window(
    windows: impl IntoIterator<Item = (u64, bool)>,
    quick: Option<u64>,
) -> Option<u64> {
    windows
        .into_iter()
        .find(|&(seq, has_tabs)| has_tabs && quick != Some(seq))
        .map(|(seq, _)| seq)
}

impl App {
    /// Save the session one last time when the app quits with windows open.
    ///
    /// The sweep in `about_to_wait_inner` writes at most every two seconds,
    /// so without this a quit loses whatever changed since. One save covers
    /// every window: the snapshot takes them all. It goes through
    /// `save_session` and so through all of its gates (restore-session off,
    /// a named layout that refuses an empty snapshot, another kettle build
    /// running, a session that failed to load). As with the sweep, it needs a
    /// window other than the quick terminal that still has tabs: closing the
    /// last window has already saved, and an empty snapshot written now
    /// would replace the one the user means to restore. A startup that failed
    /// saves nothing.
    pub(super) fn save_session_on_exit(&mut self) {
        if self.exit_teardown_done || self.startup_error.is_some() {
            return;
        }
        let Some(seq) = exit_save_window(
            self.windows
                .iter()
                .map(|(seq, ws)| (*seq, !ws.mux.tabs.is_empty())),
            self.quick_terminal.model.window,
        ) else {
            return;
        };
        // `save_session` takes one window checked out and the rest from the
        // map, as every dispatch does.
        let Some(ws) = self.windows.remove(&seq) else {
            return;
        };
        self.save_session(&ws);
        self.windows.insert(seq, ws);
    }

    /// Hand a recording still running on quit the output already read from
    /// each pane, as `close_window_now` does before it finishes the recorder.
    /// One non-blocking drain per window, without the bounded wait
    /// `flush_recorder_output` gives a pane about to be reaped: nothing is
    /// reaped here, and the quit should not wait on it.
    pub(super) fn drain_recording_on_exit(&mut self) {
        if self.exit_teardown_done || self.recorder.is_none() {
            return;
        }
        let seqs: Vec<u64> = self.windows.keys().copied().collect();
        for seq in seqs {
            let Some(mut ws) = self.windows.remove(&seq) else {
                continue;
            };
            self.drain_recorder_output_once(&mut ws);
            self.windows.insert(seq, ws);
        }
    }

    /// The exit teardown, once: from `exiting` or after `run_app` returns,
    /// whichever comes first. See [`run_exit_teardown`].
    pub(super) fn finish_exit(&mut self) {
        run_exit_teardown(
            ExitParts {
                done: &mut self.exit_teardown_done,
                recorder: self.recorder.as_mut(),
                png_encoder: &mut self.png_encoder,
                pasted_images: &mut self.pasted_images,
                viewer_copies: &self.viewer_copies,
                document_copies: &self.document_copies,
                video_copies: &self.video_copies,
                session_writer: &mut self.session_writer,
            },
            || {
                crate::notifications::flush_desktop_notifications(std::time::Duration::from_millis(
                    250,
                ))
            },
        );
    }
}

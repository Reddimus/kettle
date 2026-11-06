//! CLIPBOARD text on Wayland through the native bridge's data device.
use std::time::Instant;

use super::worker::{TextProvider, TextWorker};

/// The worker that serializes reads and writes off the UI thread. The
/// selection itself lives with the bridge (`crate::wayland::selection`), which
/// owns the one data device the compositor reports the selection to.
pub(super) fn clipboard() -> Option<TextWorker> {
    match TextWorker::start(|| Ok(Native)) {
        Ok(worker) => Some(worker),
        Err(error) => {
            log::warn!("Wayland text clipboard unavailable: {error}; falling back to arboard");
            None
        }
    }
}

struct Native;

impl TextProvider for Native {
    fn load(
        &mut self,
        primary: bool,
        max: usize,
        deadline: Instant,
    ) -> Result<String, arboard::Error> {
        if primary {
            // PRIMARY needs the primary-selection protocol; arboard covers it
            // where XWayland exists.
            return Err(arboard::Error::ContentNotAvailable);
        }
        crate::wayland::selection()
            .load(max, deadline)
            .map_err(|error| {
                // The kind maps to arboard's error; the message says which step
                // failed (no bridge, no offer, cap or deadline) for diagnosis.
                log::debug!("Wayland clipboard read failed: {error}");
                map_error(error)
            })
    }

    fn store(
        &mut self,
        primary: bool,
        text: String,
        _deadline: Instant,
    ) -> Result<(), arboard::Error> {
        if primary {
            return Err(arboard::Error::ClipboardNotSupported);
        }
        crate::wayland::selection().store(text).map_err(|error| {
            log::debug!("Wayland clipboard write failed: {error}");
            map_error(error)
        })
    }
}

fn map_error(error: std::io::Error) -> arboard::Error {
    match error.kind() {
        std::io::ErrorKind::TimedOut => arboard::Error::Timeout,
        std::io::ErrorKind::FileTooLarge | std::io::ErrorKind::OutOfMemory => {
            arboard::Error::LimitExceeded
        }
        _ => arboard::Error::ContentNotAvailable,
    }
}

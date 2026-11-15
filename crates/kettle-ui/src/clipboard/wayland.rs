//! CLIPBOARD and PRIMARY on Wayland through the native bridge's devices.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use super::worker::{PrimaryProbe, TextProvider, TextWorker};
use super::{PasteContent, PasteWant};

/// The worker that serializes reads and writes off the UI thread. The
/// selections themselves live with the bridge (`crate::wayland::selection`),
/// which owns the one data device the compositor reports the selection to.
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
    /// PRIMARY is native only where the compositor has the primary-selection
    /// protocol; elsewhere arboard covers it where XWayland exists.
    fn primary_probe(&self) -> PrimaryProbe {
        Arc::new(|| crate::wayland::selection().primary_available())
    }

    fn load(
        &mut self,
        primary: bool,
        max: usize,
        deadline: Instant,
    ) -> Result<String, arboard::Error> {
        let selection = crate::wayland::selection();
        let text = if primary {
            selection.load_primary(max, deadline)
        } else {
            selection.load(max, deadline)
        };
        text.map_err(|error| {
            // The kind maps to arboard's error; the message says which step
            // failed (no bridge, no offer, cap or deadline) for diagnosis.
            log::debug!("Wayland selection read failed: {error}");
            crate::wayland::selection_error(&error)
        })
    }

    fn store(
        &mut self,
        primary: bool,
        text: String,
        _deadline: Instant,
    ) -> Result<(), arboard::Error> {
        let selection = crate::wayland::selection();
        let stored = if primary {
            selection.store_primary(text)
        } else {
            selection.store(text)
        };
        stored.map_err(|error| {
            log::debug!("Wayland selection write failed: {error}");
            crate::wayland::selection_error(&error)
        })
    }

    fn paste(&mut self, want: PasteWant, text_max: usize, deadline: Instant) -> PasteContent {
        crate::wayland::selection().paste(want, text_max, deadline)
    }

    fn files(&mut self, deadline: Instant) -> Option<Vec<PathBuf>> {
        crate::wayland::selection().files(deadline)
    }
}

use std::sync::OnceLock;
use std::time::Instant;

use winit::event_loop::OwnedDisplayHandle;
use winit::raw_window_handle::{HasDisplayHandle, RawDisplayHandle};

use super::worker::{READ_TIMEOUT, TextProvider, TextWorker};

// Keep the process's one worker and display alive even if a compositor stalls.
static CLIPBOARD: OnceLock<Option<TextWorker>> = OnceLock::new();

pub(super) fn clipboard(display: OwnedDisplayHandle) -> Option<TextWorker> {
    CLIPBOARD
        .get_or_init(|| match TextWorker::start(move || Native::new(display)) {
            Ok(worker) => Some(worker),
            Err(error) => {
                log::warn!(
                    "Wayland text clipboard initialization failed: {error}; falling back to arboard"
                );
                None
            }
        })
        .clone()
}

pub(super) fn store_if_initialized(text: &str) -> Option<Result<(), arboard::Error>> {
    CLIPBOARD
        .get()?
        .as_ref()
        .map(|worker| worker.store(false, text.to_owned()))
}

struct Native {
    clipboard: smithay_clipboard::Clipboard,
    // Dropped after smithay and its joined queue thread; also retains the
    // connection if initialization or a load outlives the caller's deadline.
    _display: OwnedDisplayHandle,
}

impl Native {
    fn new(display: OwnedDisplayHandle) -> Result<Self, arboard::Error> {
        let handle = display
            .display_handle()
            .map_err(|_| arboard::Error::ClipboardNotSupported)?;
        let RawDisplayHandle::Wayland(raw) = handle.as_raw() else {
            return Err(arboard::Error::ClipboardNotSupported);
        };
        // SAFETY: this wl_display belongs to the process's winit event loop.
        // The owned display below retains its connection for the whole smithay
        // lifetime, including its queue thread and any read that times out.
        // smithay uses a separate queue and never disconnects the display.
        let clipboard = unsafe { smithay_clipboard::Clipboard::new(raw.display.as_ptr()) };
        let native = Self {
            clipboard,
            _display: display,
        };
        native
            .clipboard
            .wait_ready(READ_TIMEOUT)
            .map_err(map_error)?;
        Ok(native)
    }
}

impl TextProvider for Native {
    fn load(
        &mut self,
        primary: bool,
        max: usize,
        deadline: Instant,
    ) -> Result<String, arboard::Error> {
        if primary {
            self.clipboard.load_primary_bounded(max, deadline)
        } else {
            self.clipboard.load_bounded(max, deadline)
        }
        .map_err(map_error)
    }

    fn store(
        &mut self,
        primary: bool,
        text: String,
        deadline: Instant,
    ) -> Result<(), arboard::Error> {
        self.clipboard
            .store_bounded(primary, text, deadline)
            .map_err(map_error)
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

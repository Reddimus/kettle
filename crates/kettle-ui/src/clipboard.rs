//! Platform selections, and what a paste takes from the clipboard.
//!
//! One order holds on every platform: a file list, then text, then an image.
//! Text wins over an image because a source that offers both, such as
//! spreadsheet cells, means the text. On native Wayland the bridge negotiates
//! from the offer's type list (`crate::wayland`); everywhere else arboard is
//! asked in that order.
//!
//! A read is answered by whichever program owns the clipboard, and that
//! program may not answer. On Wayland and on Windows the clipboard is
//! therefore asked on one worker thread (`worker`): the UI stops waiting at
//! the read's deadline, and requests are served one at a time in the order
//! they were made, so an OSC 52 store never overtakes the load before it.
//! X11 reads carry their own deadline inside arboard. macOS is still asked on
//! the caller's thread, with no deadline that reaches into AppKit
//! (docs/AUDIT-DEFERRED.md).
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[cfg(any(target_os = "linux", test))]
use winit::raw_window_handle::RawDisplayHandle;

#[cfg(target_os = "linux")]
mod wayland;
#[cfg(any(target_os = "linux", windows, test))]
mod worker;

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum TextBackend {
    Native,
    Arboard,
}

#[cfg(any(target_os = "linux", test))]
pub(crate) fn text_backend(display: RawDisplayHandle) -> TextBackend {
    match display {
        RawDisplayHandle::Wayland(_) => TextBackend::Native,
        _ => TextBackend::Arboard,
    }
}

/// What a paste may take besides text: the `paste-files` and `paste-images`
/// policies.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct PasteWant {
    pub(crate) files: bool,
    pub(crate) images: bool,
}

/// What the clipboard gives a paste.
pub(crate) enum PasteContent {
    Files(Vec<PathBuf>),
    Image(PasteImage),
    /// The text, or why there is none. `ContentNotAvailable` is an empty
    /// clipboard; a timeout or an oversize text is said to the user.
    Text(Result<String, arboard::Error>),
    /// The clipboard holds local files, or a list too damaged to tell, and
    /// `paste-files` is off: nothing is pasted, not from the list and not
    /// from the plain-text copy of the paths that file managers add beside
    /// it, and nothing is said.
    FilesOff,
    /// Nothing is pasted and the user is told why.
    Refused(PasteRefusal),
}

pub(crate) enum PasteImage {
    /// Pixels arboard decoded.
    Rgba(arboard::ImageData<'static>),
    /// Encoded bytes from another program, untrusted and still to be decoded.
    #[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
    Png(Vec<u8>),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PasteRefusal {
    /// A malformed or oversize file list.
    #[cfg_attr(not(any(target_os = "linux", test)), allow(dead_code))]
    Files,
    /// An image that could not be read.
    Image,
}

/// The longest a clipboard read may take.
const READ_WITHIN: Duration = Duration::from_secs(2);

/// The most text a field or a preview takes from the clipboard: the size of
/// the largest paste.
const FIELD_TEXT_MAX: usize = 4 << 20;

pub(crate) struct Clipboard<F = arboard::Clipboard> {
    /// Asked on the caller's thread.
    arboard: Option<F>,
    /// Asked on a thread the caller can stop waiting for: the Wayland bridge,
    /// and on Windows arboard itself.
    #[cfg(any(target_os = "linux", windows, test))]
    worker: Option<worker::TextWorker>,
}

impl Clipboard {
    pub(crate) fn new() -> Option<Self> {
        // Windows passes a read on to the program that owns the clipboard and
        // waits for its answer inside `GetClipboardData`, with the clipboard
        // held open. No deadline reaches into that call, so it is made on the
        // worker thread.
        #[cfg(windows)]
        let clipboard = Self::serialized(arboard::Clipboard::new);
        #[cfg(not(windows))]
        let clipboard = arboard::Clipboard::new().map(|arboard| Self {
            arboard: Some(arboard),
            #[cfg(any(target_os = "linux", test))]
            worker: None,
        });
        match clipboard {
            Ok(clipboard) => Some(clipboard),
            Err(error) => {
                log::warn!("arboard clipboard unavailable: {error}");
                None
            }
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn attach_wayland(clipboard: &mut Option<Self>, window: &winit::window::Window) {
        use winit::raw_window_handle::HasDisplayHandle;
        let Ok(handle) = window.display_handle() else {
            return;
        };
        if text_backend(handle.as_raw()) != TextBackend::Native {
            return;
        }
        if let Some(worker) = wayland::clipboard() {
            let clipboard = clipboard.get_or_insert_with(|| Self {
                arboard: None,
                worker: None,
            });
            clipboard.worker = Some(worker);
        }
    }
}

/// A format-at-a-time clipboard as the worker thread asks it.
#[cfg(any(windows, test))]
struct Serialized<F>(F);

#[cfg(any(windows, test))]
impl<F: SelectionFallback + Send + 'static> worker::TextProvider for Serialized<F> {
    fn load(
        &mut self,
        primary: bool,
        max: usize,
        deadline: Instant,
    ) -> Result<String, arboard::Error> {
        self.0.load(primary, max, deadline)
    }

    fn store(
        &mut self,
        primary: bool,
        text: String,
        _deadline: Instant,
    ) -> Result<(), arboard::Error> {
        self.0.store(primary, text)
    }

    fn paste(&mut self, want: PasteWant, text_max: usize, deadline: Instant) -> PasteContent {
        // One deadline for the whole paste: a file list that came late leaves
        // the text unasked, and the thread free for the next request.
        fallback_paste(&mut self.0, want, text_max, Some(deadline))
    }

    fn files(&mut self, _deadline: Instant) -> Option<Vec<PathBuf>> {
        self.0.files().ok()
    }
}

#[cfg(any(windows, test))]
impl<F: SelectionFallback + Send + 'static> Clipboard<F> {
    /// A format-at-a-time clipboard that is created and asked on the worker
    /// thread only. Its owner can keep that thread waiting, not the caller:
    /// every request returns by its deadline, and a request made while the
    /// thread waits is served after the one before it or refused, never
    /// ahead of it.
    fn serialized(
        create: impl FnOnce() -> Result<F, arboard::Error> + Send + 'static,
    ) -> Result<Self, arboard::Error> {
        let worker = worker::TextWorker::start(move || create().map(Serialized))?;
        Ok(Self {
            arboard: None,
            worker: Some(worker),
        })
    }
}

/// The platform selection boundary, injectable without opening an OS clipboard.
pub(crate) trait SelectionFallback {
    fn store(&mut self, primary: bool, text: String) -> Result<(), arboard::Error>;
    fn load(
        &mut self,
        primary: bool,
        max: usize,
        deadline: Instant,
    ) -> Result<String, arboard::Error>;
    /// The CLIPBOARD file list; empty when it holds none.
    fn files(&mut self) -> Result<Vec<PathBuf>, arboard::Error>;
    /// The CLIPBOARD bitmap, decoded.
    fn image(&mut self) -> Result<arboard::ImageData<'static>, arboard::Error>;
}

impl SelectionFallback for arboard::Clipboard {
    fn store(&mut self, primary: bool, text: String) -> Result<(), arboard::Error> {
        #[cfg(target_os = "linux")]
        if primary {
            use arboard::{LinuxClipboardKind, SetExtLinux};
            return self.set().clipboard(LinuxClipboardKind::Primary).text(text);
        }
        let _ = primary;
        self.set_text(text)
    }

    fn load(
        &mut self,
        primary: bool,
        max: usize,
        deadline: Instant,
    ) -> Result<String, arboard::Error> {
        #[cfg(target_os = "linux")]
        if primary {
            use arboard::{GetExtLinux, LinuxClipboardKind};
            return self
                .get()
                .clipboard(LinuxClipboardKind::Primary)
                .text_bounded(max, deadline);
        }
        let _ = primary;
        self.get_text_bounded(max, deadline)
    }

    fn files(&mut self) -> Result<Vec<PathBuf>, arboard::Error> {
        self.get().file_list()
    }

    fn image(&mut self) -> Result<arboard::ImageData<'static>, arboard::Error> {
        self.get_image()
    }
}

/// What a paste takes from a clipboard that is asked one format at a time.
/// The text is read by `text_by`, or within `READ_WITHIN` of being asked for.
fn fallback_paste(
    clipboard: &mut impl SelectionFallback,
    want: PasteWant,
    text_max: usize,
    text_by: Option<Instant>,
) -> PasteContent {
    // The list is read with `paste-files` off as well: copied files must
    // then paste nothing, rather than the text copy of their paths. A list
    // read that fails is treated as no list. On X11 a failure does not show
    // that a list exists: xclip answers every request with its own type, an
    // owner may leave a request it cannot serve unanswered, and both still
    // have text to paste.
    if let Ok(paths) = clipboard.files()
        && !paths.is_empty()
    {
        return if want.files {
            PasteContent::Files(paths)
        } else {
            PasteContent::FilesOff
        };
    }
    let text = match text_by {
        Some(deadline) if Instant::now() >= deadline => Err(arboard::Error::Timeout),
        _ => clipboard.load(
            false,
            text_max,
            text_by.unwrap_or_else(|| Instant::now() + READ_WITHIN),
        ),
    };
    match &text {
        Ok(text) if text.is_empty() => {}
        // A text that timed out or is too large is still the text.
        Ok(_) | Err(arboard::Error::Timeout | arboard::Error::LimitExceeded) => {
            return PasteContent::Text(text);
        }
        Err(_) => {}
    }
    // A paste past its deadline asks the owner for nothing more: nobody waits
    // for the image, and the next request waits for the thread.
    if text_by.is_some_and(|deadline| Instant::now() >= deadline) {
        return PasteContent::Text(Err(arboard::Error::Timeout));
    }
    if want.images {
        match clipboard.image() {
            Ok(image) => return PasteContent::Image(PasteImage::Rgba(image)),
            // An image is there and could not be had: over the read cap or
            // the decode limits, undecodable, or never sent. That is said,
            // as the Wayland route says it. Any other error is a clipboard
            // with no image, which pastes nothing quietly.
            Err(arboard::Error::LimitExceeded | arboard::Error::ConversionFailure) => {
                return PasteContent::Refused(PasteRefusal::Image);
            }
            Err(arboard::Error::Timeout) => {
                return PasteContent::Text(Err(arboard::Error::Timeout));
            }
            Err(_) => {}
        }
    }
    PasteContent::Text(text)
}

// PRIMARY is native where the compositor has the primary-selection protocol.
// Where it does not, that channel stays on arboard (XWayland); native
// CLIPBOARD errors are never treated as a reason to fall back.
impl<F: SelectionFallback> Clipboard<F> {
    /// What a paste takes from CLIPBOARD under `want`, with text capped at
    /// `text_max` bytes. On Wayland the bridge is the authority and arboard is
    /// not asked as well: an X11-owned selection reaches a focused Wayland
    /// client as an ordinary offer, and a second probe would only double the
    /// worst-case wait.
    pub(crate) fn paste_content(&mut self, want: PasteWant, text_max: usize) -> PasteContent {
        #[cfg(any(target_os = "linux", windows, test))]
        if let Some(worker) = &self.worker {
            return worker.paste(want, text_max, Instant::now() + READ_WITHIN);
        }
        match self.arboard.as_mut() {
            Some(clipboard) => fallback_paste(clipboard, want, text_max, None),
            None => PasteContent::Text(Err(arboard::Error::ContentNotAvailable)),
        }
    }

    /// CLIPBOARD text for a field or a preview, under a paste's bounds: no
    /// more than the largest paste, and no longer than a paste may wait. On
    /// macOS both are checked only once AppKit has returned the text
    /// (docs/AUDIT-DEFERRED.md).
    pub(crate) fn get_text(&mut self) -> Result<String, arboard::Error> {
        self.get_selection_bounded(false, FIELD_TEXT_MAX, Instant::now() + READ_WITHIN)
    }

    /// The local files CLIPBOARD names, for a preview of what a paste brings.
    pub(crate) fn file_list(&mut self) -> Result<Vec<PathBuf>, arboard::Error> {
        #[cfg(any(target_os = "linux", windows, test))]
        if let Some(worker) = &self.worker {
            return worker.files(Instant::now() + READ_WITHIN);
        }
        self.arboard
            .as_mut()
            .ok_or(arboard::Error::ContentNotAvailable)?
            .files()
    }

    pub(crate) fn set_text(&mut self, text: String) -> Result<(), arboard::Error> {
        self.set_selection(false, text)
    }

    pub(crate) fn set_selection(
        &mut self,
        primary: bool,
        text: String,
    ) -> Result<(), arboard::Error> {
        #[cfg(any(target_os = "linux", windows, test))]
        if let Some(worker) = &self.worker
            && (!primary || worker.supports_primary())
        {
            return worker.store(primary, text);
        }
        let clipboard = self
            .arboard
            .as_mut()
            .ok_or(arboard::Error::ContentNotAvailable)?;
        clipboard.store(primary, text)
    }

    pub(crate) fn get_selection_bounded(
        &mut self,
        primary: bool,
        max: usize,
        deadline: Instant,
    ) -> Result<String, arboard::Error> {
        #[cfg(any(target_os = "linux", windows, test))]
        if let Some(worker) = &self.worker
            && (!primary || worker.supports_primary())
        {
            return worker.load(primary, max, deadline);
        }
        let clipboard = self
            .arboard
            .as_mut()
            .ok_or(arboard::Error::ContentNotAvailable)?;
        clipboard.load(primary, max, deadline)
    }
}

/// Store text through the Wayland bridge when it is connected; `None` leaves
/// the caller to use arboard.
#[cfg(target_os = "linux")]
pub(crate) fn store_wayland_text(text: &str) -> Option<Result<(), arboard::Error>> {
    let selection = crate::wayland::selection();
    selection.available().then(|| {
        selection
            .store(text.to_owned())
            .map_err(|_| arboard::Error::ContentNotAvailable)
    })
}

#[cfg(test)]
mod tests;

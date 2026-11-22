//! Platform selections, and what a paste takes from the clipboard.
//!
//! One order holds on every platform: a file list, then text, then an image.
//! Text wins over an image because a source that offers both, such as
//! spreadsheet cells, means the text. On native Wayland the bridge negotiates
//! from the offer's type list (`crate::wayland`); everywhere else arboard is
//! asked in that order.
//!
//! A read is answered by whichever program owns the clipboard, and that
//! program may not answer. On Wayland, macOS and Windows the clipboard is
//! therefore asked on one worker thread (`worker`): the UI stops waiting at
//! the read's deadline, and requests are served one at a time in the order
//! they were made, so an OSC 52 store never overtakes the load before it.
//! X11 reads carry their own deadline inside arboard. A thread bounds the
//! wait, not the memory: macOS and Windows hand over a whole flavor before
//! its size can be checked (docs/AUDIT-DEFERRED.md).
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[cfg(any(target_os = "linux", test))]
use winit::raw_window_handle::RawDisplayHandle;

#[cfg(target_os = "linux")]
mod wayland;
#[cfg(any(target_os = "linux", target_os = "macos", windows, test))]
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

/// The longest the window waits for a clipboard read.
const READ_WITHIN: Duration = Duration::from_secs(2);

/// How long the answer to a read the user asked for is still taken on macOS
/// after the window has stopped waiting for it. The user may be part of the
/// read there: answering the system's paste-permission alert, or waiting for
/// a transfer from another device. A program's read (OSC 52) keeps
/// `READ_WITHIN`.
#[cfg(target_os = "macos")]
const LATE_ANSWER_WITHIN: Duration = Duration::from_secs(60);

/// The most text a field or a preview takes from the clipboard: the size of
/// the largest paste.
const FIELD_TEXT_MAX: usize = 4 << 20;

pub(crate) struct Clipboard<F = arboard::Clipboard> {
    /// Asked on the caller's thread.
    arboard: Option<F>,
    /// Asked on a thread the caller can stop waiting for: the Wayland bridge,
    /// and on macOS and Windows arboard itself.
    #[cfg(any(target_os = "linux", target_os = "macos", windows, test))]
    worker: Option<worker::TextWorker>,
    /// How long after `READ_WITHIN` the worker's answer to a read the user
    /// asked for is still taken; `None` drops it at `READ_WITHIN`.
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos", windows, test)),
        allow(dead_code)
    )]
    late_answers: Option<Duration>,
}

/// What a preview found on CLIPBOARD (`Clipboard::peek`).
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct ClipboardPeek {
    pub(crate) files: Vec<PathBuf>,
    pub(crate) text: Option<String>,
}

/// A read the user asked for: answered within `READ_WITHIN`, or still to
/// come.
pub(crate) enum UserRead<T> {
    Now(T),
    Later(LateAnswer<T>),
}

/// The answer to a read the window stopped waiting for. It is taken when it
/// comes, and is a timeout once its deadline has passed. Dropping it before
/// the worker has taken the read leaves the read unasked.
pub(crate) struct LateAnswer<T> {
    answer: crossbeam_channel::Receiver<T>,
    deadline: Instant,
    timed_out: fn() -> T,
    _interest: std::sync::Arc<()>,
}

impl<T> LateAnswer<T> {
    #[cfg_attr(
        not(any(target_os = "linux", target_os = "macos", windows, test)),
        allow(dead_code)
    )]
    fn new(
        answer: crossbeam_channel::Receiver<T>,
        deadline: Instant,
        timed_out: fn() -> T,
        interest: std::sync::Arc<()>,
    ) -> Self {
        Self {
            answer,
            deadline,
            timed_out,
            _interest: interest,
        }
    }

    /// The answer once it has come or its deadline has passed; `None` while
    /// it is still awaited.
    pub(crate) fn poll(&self, now: Instant) -> Option<T> {
        match self.answer.try_recv() {
            Ok(answer) => Some(answer),
            Err(crossbeam_channel::TryRecvError::Empty) if now < self.deadline => None,
            Err(_) => Some((self.timed_out)()),
        }
    }

    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
}

#[cfg(test)]
impl<T> LateAnswer<T> {
    /// An answer still to come by `deadline`, sent through the returned
    /// sender.
    pub(crate) fn pending(
        deadline: Instant,
        timed_out: fn() -> T,
    ) -> (crossbeam_channel::Sender<T>, Self) {
        let (sender, answer) = crossbeam_channel::bounded(1);
        (
            sender,
            Self::new(answer, deadline, timed_out, std::sync::Arc::new(())),
        )
    }
}

#[cfg(test)]
impl Clipboard {
    /// A clipboard whose CLIPBOARD text is what `load` answers, asked on the
    /// worker thread, for tests of the code that pastes. `late_answers` is as
    /// on macOS when set.
    pub(crate) fn answering(
        load: impl FnMut() -> Result<String, arboard::Error> + Send + 'static,
        late_answers: Option<Duration>,
    ) -> Self {
        struct Answering<L>(L);
        impl<L: FnMut() -> Result<String, arboard::Error> + Send + 'static> worker::TextProvider
            for Answering<L>
        {
            fn load(&mut self, _: bool, _: usize, _: Instant) -> Result<String, arboard::Error> {
                (self.0)()
            }
            fn store(&mut self, _: bool, _: String, _: Instant) -> Result<(), arboard::Error> {
                Ok(())
            }
        }
        Self {
            arboard: None,
            worker: Some(worker::TextWorker::start(move || Ok(Answering(load))).unwrap()),
            late_answers,
        }
    }
}

impl Clipboard {
    pub(crate) fn new() -> Option<Self> {
        match Self::open(arboard::Clipboard::new) {
            Ok(clipboard) => Some(clipboard),
            Err(error) => {
                log::warn!("arboard clipboard unavailable: {error}");
                None
            }
        }
    }

    /// The clipboard `create` opens, asked where this platform must ask it.
    /// Windows passes a read on to the program that owns the clipboard and
    /// waits for its answer inside `GetClipboardData`, with the clipboard
    /// held open; AppKit waits inside `stringForType:` for an owner that
    /// provides its data lazily. No deadline reaches into either call, so on
    /// both the clipboard is created and asked on the worker thread.
    ///
    /// On macOS the answer to a read the user asked for is still taken for
    /// `LATE_ANSWER_WITHIN` after the window stops waiting: there the system
    /// can ask the user to allow the paste, and a read Kettle gave up on at
    /// two seconds would lose the paste the user then allowed.
    #[cfg(any(target_os = "macos", windows))]
    fn open(
        create: impl FnOnce() -> Result<arboard::Clipboard, arboard::Error> + Send + 'static,
    ) -> Result<Self, arboard::Error> {
        let clipboard = Self::serialized(create)?;
        #[cfg(target_os = "macos")]
        let clipboard = Self {
            late_answers: Some(LATE_ANSWER_WITHIN),
            ..clipboard
        };
        Ok(clipboard)
    }

    /// Elsewhere arboard is asked on the caller's thread: X11 reads carry
    /// their own deadline, and Wayland attaches its bridge as the worker.
    #[cfg(not(any(target_os = "macos", windows)))]
    fn open(
        create: impl FnOnce() -> Result<arboard::Clipboard, arboard::Error>,
    ) -> Result<Self, arboard::Error> {
        create().map(|arboard| Self {
            arboard: Some(arboard),
            #[cfg(any(target_os = "linux", test))]
            worker: None,
            late_answers: None,
        })
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
                late_answers: None,
            });
            clipboard.worker = Some(worker);
        }
    }
}

/// A format-at-a-time clipboard as the worker thread asks it.
#[cfg(any(target_os = "macos", windows, test))]
struct Serialized<F>(F);

/// Runs one request. On macOS its AppKit objects are released when it
/// returns: the worker thread lives as long as the window and has no pool of
/// its own to drain.
#[cfg(target_os = "macos")]
fn one_request<T>(request: impl FnOnce() -> T) -> T {
    objc2_06::rc::autoreleasepool(|_| request())
}

#[cfg(all(not(target_os = "macos"), any(windows, test)))]
fn one_request<T>(request: impl FnOnce() -> T) -> T {
    request()
}

#[cfg(any(target_os = "macos", windows, test))]
impl<F: SelectionFallback + Send + 'static> worker::TextProvider for Serialized<F> {
    fn load(
        &mut self,
        primary: bool,
        max: usize,
        deadline: Instant,
    ) -> Result<String, arboard::Error> {
        one_request(|| self.0.load(primary, max, deadline))
    }

    fn store(
        &mut self,
        primary: bool,
        text: String,
        _deadline: Instant,
    ) -> Result<(), arboard::Error> {
        one_request(|| self.0.store(primary, text))
    }

    fn paste(&mut self, want: PasteWant, text_max: usize, deadline: Instant) -> PasteContent {
        self.paste_while(want, text_max, deadline, &|| true)
    }

    fn paste_while(
        &mut self,
        want: PasteWant,
        text_max: usize,
        deadline: Instant,
        wanted: &dyn Fn() -> bool,
    ) -> PasteContent {
        // One deadline for the whole paste: a file list that came late leaves
        // the text unasked, and the thread free for the next request. A paste
        // nobody waits for any more, one the user stopped waiting for, asks
        // for no further format either.
        one_request(|| fallback_paste(&mut self.0, want, text_max, Some(deadline), wanted))
    }

    fn files(&mut self, _deadline: Instant) -> Option<Vec<PathBuf>> {
        one_request(|| self.0.files().ok())
    }
}

#[cfg(any(target_os = "macos", windows, test))]
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
            late_answers: None,
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
/// Once `wanted` says nobody waits for the paste, no further format is asked
/// for.
fn fallback_paste(
    clipboard: &mut impl SelectionFallback,
    want: PasteWant,
    text_max: usize,
    text_by: Option<Instant>,
    wanted: &dyn Fn() -> bool,
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
        _ if !wanted() => Err(arboard::Error::Timeout),
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
    if !wanted() || text_by.is_some_and(|deadline| Instant::now() >= deadline) {
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
        #[cfg(any(target_os = "linux", target_os = "macos", windows, test))]
        if let Some(worker) = &self.worker {
            return worker.paste(want, text_max, Instant::now() + READ_WITHIN);
        }
        match self.arboard.as_mut() {
            Some(clipboard) => fallback_paste(clipboard, want, text_max, None, &|| true),
            None => PasteContent::Text(Err(arboard::Error::ContentNotAvailable)),
        }
    }

    /// CLIPBOARD text for a field or a preview, under a paste's bounds: no
    /// more than the largest paste, and no longer than a paste may wait. On
    /// macOS and Windows the size is checked only once the system has handed
    /// the text over (docs/AUDIT-DEFERRED.md).
    pub(crate) fn get_text(&mut self) -> Result<String, arboard::Error> {
        self.get_selection_bounded(false, FIELD_TEXT_MAX, Instant::now() + READ_WITHIN)
    }

    /// [`Self::paste_content`] for a paste the user asked for. Where late
    /// answers are taken (macOS), an answer that has not come within
    /// `READ_WITHIN` is left to come later instead of timing out.
    pub(crate) fn paste_for_user(
        &mut self,
        want: PasteWant,
        text_max: usize,
    ) -> UserRead<PasteContent> {
        #[cfg(any(target_os = "linux", target_os = "macos", windows, test))]
        if let (Some(worker), Some(late)) = (&self.worker, self.late_answers) {
            let now = Instant::now();
            return worker.paste_later(want, text_max, now + READ_WITHIN, now + READ_WITHIN + late);
        }
        UserRead::Now(self.paste_content(want, text_max))
    }

    /// [`Self::get_text`] for a field the user pastes into, waited for as
    /// [`Self::paste_for_user`] waits.
    pub(crate) fn text_for_user(&mut self) -> UserRead<Result<String, arboard::Error>> {
        #[cfg(any(target_os = "linux", target_os = "macos", windows, test))]
        if let (Some(worker), Some(late)) = (&self.worker, self.late_answers) {
            let now = Instant::now();
            return worker.load_later(FIELD_TEXT_MAX, now + READ_WITHIN, now + READ_WITHIN + late);
        }
        UserRead::Now(self.get_text())
    }

    /// The local files CLIPBOARD names, as a preview reads them.
    #[cfg(test)]
    pub(crate) fn file_list(&mut self) -> Result<Vec<PathBuf>, arboard::Error> {
        self.file_list_by(Instant::now() + READ_WITHIN)
    }

    /// The local files CLIPBOARD names, by `deadline`.
    fn file_list_by(&mut self, deadline: Instant) -> Result<Vec<PathBuf>, arboard::Error> {
        #[cfg(any(target_os = "linux", target_os = "macos", windows, test))]
        if let Some(worker) = &self.worker {
            return worker.files(deadline);
        }
        let _ = deadline;
        self.arboard
            .as_mut()
            .ok_or(arboard::Error::ContentNotAvailable)?
            .files()
    }

    /// What CLIPBOARD holds for a preview: the files it names, and its text,
    /// read as a paste reads them. The text is read only when no files are
    /// named, or with `text_with_files`.
    ///
    /// On the worker (Wayland, macOS, Windows) both reads share one paste's
    /// deadline, and a list that timed out leaves the text unasked, so a slow
    /// owner holds a preview for `READ_WITHIN` and no longer. Asked on the
    /// caller's thread (X11), a list read that fails, a timeout included, is
    /// no list, and the text has its own `READ_WITHIN`, as `fallback_paste`
    /// gives it: there a failure does not show a list exists, and an owner may
    /// leave a list request unanswered and still have text to paste.
    pub(crate) fn peek(&mut self, text_with_files: bool) -> ClipboardPeek {
        #[cfg(any(target_os = "linux", target_os = "macos", windows, test))]
        let shared = self.worker.is_some();
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows, test)))]
        let shared = false;
        let deadline = Instant::now() + READ_WITHIN;
        let files = match self.file_list_by(deadline) {
            Ok(files) => files,
            Err(arboard::Error::Timeout) if shared => return ClipboardPeek::default(),
            Err(_) => Vec::new(),
        };
        let text_by = if shared {
            deadline
        } else {
            Instant::now() + READ_WITHIN
        };
        let text = (files.is_empty() || text_with_files)
            .then(|| {
                self.get_selection_bounded(false, FIELD_TEXT_MAX, text_by)
                    .ok()
            })
            .flatten();
        ClipboardPeek { files, text }
    }

    pub(crate) fn set_text(&mut self, text: String) -> Result<(), arboard::Error> {
        self.set_selection(false, text)
    }

    pub(crate) fn set_selection(
        &mut self,
        primary: bool,
        text: String,
    ) -> Result<(), arboard::Error> {
        #[cfg(any(target_os = "linux", target_os = "macos", windows, test))]
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
        #[cfg(any(target_os = "linux", target_os = "macos", windows, test))]
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

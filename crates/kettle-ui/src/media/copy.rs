//! Copying what a preview lane shows to the clipboard, off the UI thread.
//!
//! One thread, started on the first copy, owns its own clipboard handle for
//! the rest of the process: on X11 what was copied stays on the clipboard
//! only while its owner lives. It takes one copy at a time, and one more may
//! wait; a copy asked for past that is refused at once, never queued without
//! bound, so a clipboard owner that stalls holds up neither the UI nor
//! memory.

use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender, TrySendError};

/// What to copy.
pub(crate) enum CopyContent {
    /// An image's straight RGBA, shared with the item that holds it.
    Image(kettle_core::ImageData),
    /// A source, as it was rendered: the item's own charged text, shared.
    Text(Arc<super::source::ChargedBytes>),
}

/// A copy that ended, for the lane of `pane` in window `window`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CopyDone {
    pub window: u64,
    pub pane: u64,
    pub image: bool,
    pub copied: bool,
}

struct CopyRequest {
    window: u64,
    pane: u64,
    content: CopyContent,
}

/// The clipboard thread's two channels.
pub(crate) struct CopyService {
    work: Sender<CopyRequest>,
    done: Receiver<CopyDone>,
}

/// Whether a copy was taken.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CopyStarted {
    Started,
    /// One copy runs and another waits already.
    Busy,
    /// The clipboard thread could not start, or has stopped.
    Unavailable,
}

impl CopyService {
    /// Start the thread, which copies with `set` and runs `wake` after each
    /// copy. `set` stands for the platform clipboard; tests give their own.
    pub(crate) fn start(
        set: impl FnMut(&CopyContent) -> bool + Send + 'static,
        wake: Arc<dyn Fn() + Send + Sync>,
    ) -> Option<Self> {
        let (work, requests) = crossbeam_channel::bounded::<CopyRequest>(1);
        let (finished, done) = crossbeam_channel::bounded(2);
        let mut set = set;
        std::thread::Builder::new()
            .name("kettle-preview-copy".into())
            .spawn(move || {
                for request in requests {
                    let copied = set(&request.content);
                    let done = CopyDone {
                        window: request.window,
                        pane: request.pane,
                        image: matches!(request.content, CopyContent::Image(_)),
                        copied,
                    };
                    // The pixels or text go before the wait for the next.
                    drop(request);
                    if finished.send(done).is_err() {
                        return;
                    }
                    wake();
                }
            })
            .ok()?;
        Some(Self { work, done })
    }

    /// The thread on the platform clipboard.
    pub(crate) fn with_platform_clipboard(wake: Arc<dyn Fn() + Send + Sync>) -> Option<Self> {
        let mut clipboard = None::<arboard::Clipboard>;
        Self::start(
            move |content| {
                if clipboard.is_none() {
                    clipboard = arboard::Clipboard::new().ok();
                }
                let Some(clipboard) = clipboard.as_mut() else {
                    return false;
                };
                match content {
                    CopyContent::Image(image) => clipboard
                        .set_image(arboard::ImageData {
                            width: image.width as usize,
                            height: image.height as usize,
                            bytes: std::borrow::Cow::Borrowed(image.rgba.as_slice()),
                        })
                        .is_ok(),
                    CopyContent::Text(text) => std::str::from_utf8(text.as_slice())
                        .is_ok_and(|text| clipboard.set_text(text).is_ok()),
                }
            },
            wake,
        )
    }

    /// Copy `content` for `pane`'s lane in window `window`.
    pub(crate) fn copy(&self, window: u64, pane: u64, content: CopyContent) -> CopyStarted {
        match self.work.try_send(CopyRequest {
            window,
            pane,
            content,
        }) {
            Ok(()) => CopyStarted::Started,
            Err(TrySendError::Full(_)) => CopyStarted::Busy,
            Err(TrySendError::Disconnected(_)) => CopyStarted::Unavailable,
        }
    }

    /// The copies that ended since this was last asked.
    pub(crate) fn take_done(&self) -> Vec<CopyDone> {
        self.done.try_iter().collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_wake() -> Arc<dyn Fn() + Send + Sync> {
        Arc::new(|| {})
    }

    fn text(text: &str) -> CopyContent {
        CopyContent::Text(Arc::new(
            super::super::source::ChargedBytes::new(text.as_bytes().to_vec()).unwrap(),
        ))
    }

    /// A copy runs on the thread, says which lane and whether it worked,
    /// and wakes the App; one copy runs and one waits, and a third is
    /// refused at once while the clipboard is held up, never blocking.
    #[test]
    fn copies_run_off_the_ui_thread_one_running_and_one_waiting() {
        let (release, held) = crossbeam_channel::bounded::<()>(0);
        let (woken_tx, woken) = crossbeam_channel::unbounded::<()>();
        let service = CopyService::start(
            move |content| {
                let _ = held.recv();
                matches!(content, CopyContent::Text(text) if text.as_slice() == b"flowchart LR")
            },
            Arc::new(move || {
                let _ = woken_tx.send(());
            }),
        )
        .unwrap();
        let image = kettle_core::ImageData::new(1, 1, vec![0, 0, 0, 255]).unwrap();
        assert_eq!(
            service.copy(1, 7, text("flowchart LR")),
            CopyStarted::Started
        );
        // Until the thread takes the first, the channel holds it; give it a
        // moment to, then one more may wait and a third is refused.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut second = service.copy(1, 8, CopyContent::Image(image.clone()));
        while second == CopyStarted::Busy && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
            second = service.copy(1, 8, CopyContent::Image(image.clone()));
        }
        assert_eq!(second, CopyStarted::Started);
        assert_eq!(service.copy(1, 9, text("x")), CopyStarted::Busy);
        release.send(()).unwrap();
        release.send(()).unwrap();
        woken.recv().unwrap();
        woken.recv().unwrap();
        let mut done = service.take_done();
        done.sort_by_key(|done| done.pane);
        assert_eq!(
            done,
            [
                CopyDone {
                    window: 1,
                    pane: 7,
                    image: false,
                    copied: true
                },
                CopyDone {
                    window: 1,
                    pane: 8,
                    image: true,
                    copied: false
                },
            ]
        );
        assert!(service.take_done().is_empty(), "each told once");
    }

    /// A thread that stopped refuses at once.
    #[test]
    fn a_stopped_thread_refuses_copies() {
        let service = CopyService::start(|_| panic!("the clipboard fails"), no_wake()).unwrap();
        assert_eq!(service.copy(1, 1, text("a")), CopyStarted::Started);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        loop {
            match service.copy(1, 1, text("b")) {
                CopyStarted::Unavailable => break,
                _ if std::time::Instant::now() > deadline => panic!("never refused"),
                _ => std::thread::sleep(std::time::Duration::from_millis(5)),
            }
        }
    }
}

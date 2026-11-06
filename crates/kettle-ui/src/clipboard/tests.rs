use super::*;
use std::time::{Duration, Instant};
use worker::{TextProvider, TextWorker};

// This is the incremental collector used inside smithay's pipe callback,
// compiled on every host without the Wayland dependency.
#[rustfmt::skip]
#[path = "../../../../vendor/smithay-clipboard-0.7.2/src/bounded.rs"]
mod bounded;

#[test]
fn backend_choice_uses_the_actual_display() {
    use winit::raw_window_handle::*;
    assert_eq!(
        text_backend(RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
            std::ptr::NonNull::dangling()
        ))),
        TextBackend::Smithay
    );
    assert_eq!(
        text_backend(RawDisplayHandle::Xlib(XlibDisplayHandle::new(None, 0))),
        TextBackend::Arboard
    );
    assert_eq!(
        text_backend(RawDisplayHandle::Xcb(XcbDisplayHandle::new(None, 0))),
        TextBackend::Arboard
    );
    assert_eq!(
        text_backend(RawDisplayHandle::AppKit(AppKitDisplayHandle::new())),
        TextBackend::Arboard
    );
    assert_eq!(
        text_backend(RawDisplayHandle::Windows(WindowsDisplayHandle::new())),
        TextBackend::Arboard
    );
}

#[derive(Default)]
struct Provider {
    selections: [String; 2],
    thread: Option<std::thread::ThreadId>,
}
impl TextProvider for Provider {
    fn load(&mut self, primary: bool, _: usize, _: Instant) -> Result<String, arboard::Error> {
        assert_ne!(self.thread, Some(std::thread::current().id()));
        Ok(self.selections[usize::from(primary)].clone())
    }
    fn store(&mut self, primary: bool, text: String, _: Instant) -> Result<(), arboard::Error> {
        assert_ne!(self.thread, Some(std::thread::current().id()));
        self.selections[usize::from(primary)] = text;
        Ok(())
    }
}

fn clipboard() -> Clipboard {
    let thread = Some(std::thread::current().id());
    Clipboard {
        arboard: None,
        wayland: Some(
            TextWorker::start(move || {
                Ok(Provider {
                    thread,
                    ..Default::default()
                })
            })
            .unwrap(),
        ),
    }
}

#[test]
fn native_text_without_arboard_keeps_channels_order_caps_and_media_fallback() {
    let mut clipboard = clipboard();
    for (primary, text) in [(false, "clipboard"), (true, "primary"), (false, "next")] {
        clipboard.set_selection(primary, text.into()).unwrap();
        assert_eq!(
            clipboard
                .get_selection_bounded(primary, text.len(), Instant::now() + worker::READ_TIMEOUT)
                .unwrap(),
            text
        );
        assert!(matches!(
            clipboard.get_selection_bounded(
                primary,
                text.len() - 1,
                Instant::now() + worker::READ_TIMEOUT
            ),
            Err(arboard::Error::LimitExceeded)
        ));
    }
    assert_eq!(clipboard.get_text().unwrap(), "next");
    assert!(matches!(
        clipboard.get_selection_bounded(false, 100, Instant::now()),
        Err(arboard::Error::Timeout)
    ));
    assert!(matches!(
        clipboard.get_image(),
        Err(arboard::Error::ContentNotAvailable)
    ));
    assert!(matches!(
        clipboard.file_list(),
        Err(arboard::Error::ContentNotAvailable)
    ));
}

#[test]
fn native_read_deadline_does_not_wait_for_a_stalled_provider_or_mix_replies() {
    let (entered_tx, entered) = crossbeam_channel::bounded(1);
    let (release, blocked) = crossbeam_channel::bounded(1);
    struct Stalled {
        entered: crossbeam_channel::Sender<()>,
        release: crossbeam_channel::Receiver<()>,
        first: bool,
    }
    impl TextProvider for Stalled {
        fn load(&mut self, _: bool, _: usize, _: Instant) -> Result<String, arboard::Error> {
            if self.first {
                self.first = false;
                self.entered.send(()).unwrap();
                self.release.recv_timeout(Duration::from_secs(5)).unwrap();
                Ok("stale".into())
            } else {
                Ok("fresh".into())
            }
        }
        fn store(&mut self, _: bool, _: String, _: Instant) -> Result<(), arboard::Error> {
            Ok(())
        }
    }
    let worker = TextWorker::start(move || {
        Ok(Stalled {
            entered: entered_tx,
            release: blocked,
            first: true,
        })
    })
    .unwrap();
    let reader = worker.clone();
    let started = Instant::now();
    let join = std::thread::spawn(move || {
        reader.load(false, 100, Instant::now() + Duration::from_millis(100))
    });
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(join.join().unwrap(), Err(arboard::Error::Timeout)));
    assert!(started.elapsed() < Duration::from_secs(1));
    release.send(()).unwrap();
    assert_eq!(
        worker
            .load(true, 100, Instant::now() + worker::READ_TIMEOUT)
            .unwrap(),
        "fresh"
    );
}

#[test]
fn native_initialization_failure_is_reported_for_arboard_fallback() {
    assert!(matches!(
        TextWorker::start::<Provider>(|| Err(arboard::Error::ClipboardNotSupported)),
        Err(arboard::Error::ClipboardNotSupported)
    ));
}

#[test]
fn native_pipe_collector_enforces_caps_before_collecting_and_bounds_lossy_utf8() {
    use std::io::Cursor;
    for (bytes, max, expected) in [
        (b"abcd".as_slice(), 4, Some("abcd")),
        (b"abcde".as_slice(), 4, None),
        (b"".as_slice(), 0, Some("")),
        (b"x".as_slice(), 0, None),
        (b"\xff".as_slice(), 1, None),
        (b"\xff".as_slice(), 3, Some("\u{fffd}")),
    ] {
        let mut transfer = bounded::Transfer::new(max, Instant::now() + worker::READ_TIMEOUT);
        let mut reader = Cursor::new(bytes);
        match expected {
            Some(text) => assert_eq!(
                transfer.read_ready(&mut reader).unwrap().as_deref(),
                Some(text)
            ),
            None => assert_eq!(
                transfer.read_ready(&mut reader).unwrap_err().kind(),
                std::io::ErrorKind::FileTooLarge
            ),
        }
        assert!(reader.position() <= (max + 1) as u64);
    }
}

#[test]
fn native_pipe_collector_checks_an_absolute_deadline_even_with_continuous_data() {
    use std::io::{self, Read};
    struct Trickle;
    impl Read for Trickle {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            std::thread::sleep(Duration::from_millis(5));
            buffer[0] = b'x';
            Ok(1)
        }
    }
    let mut transfer = bounded::Transfer::new(1000, Instant::now() + Duration::from_millis(25));
    assert_eq!(
        transfer.read_ready(&mut Trickle).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
    struct Idle;
    impl Read for Idle {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::WouldBlock.into())
        }
    }
    let mut transfer = bounded::Transfer::new(10, Instant::now() + Duration::from_millis(10));
    assert_eq!(transfer.read_ready(&mut Idle).unwrap(), None);
    std::thread::sleep(Duration::from_millis(15));
    assert_eq!(
        transfer.read_ready(&mut Idle).unwrap_err().kind(),
        io::ErrorKind::TimedOut
    );
}

#[test]
fn smithay_dependency_and_adapter_stay_linux_only() {
    let manifest = include_str!("../../Cargo.toml");
    let linux = manifest
        .split("[target.'cfg(target_os = \"linux\")'.dependencies]")
        .nth(1)
        .unwrap()
        .split("[target.")
        .next()
        .unwrap();
    assert!(linux.contains("smithay-clipboard ="));
    assert_eq!(manifest.matches("smithay-clipboard =").count(), 1);
    let module = include_str!("../clipboard.rs");
    assert!(module.contains("#[cfg(target_os = \"linux\")]\nmod wayland;"));
}

#[test]
fn upstream_unbounded_collector_keeps_its_behavior() {
    let mut transfer = bounded::Transfer::unbounded();
    assert_eq!(
        transfer
            .read_ready(&mut std::io::Cursor::new(b"upstream"))
            .unwrap()
            .as_deref(),
        Some("upstream")
    );
}

#[test]
fn native_worker_refuses_a_full_queue_without_spawning_more_readers() {
    let (entered_tx, entered) = crossbeam_channel::bounded(1);
    let (release, blocked) = crossbeam_channel::bounded(1);
    struct Held {
        entered: crossbeam_channel::Sender<()>,
        release: crossbeam_channel::Receiver<()>,
    }
    impl TextProvider for Held {
        fn load(&mut self, _: bool, _: usize, _: Instant) -> Result<String, arboard::Error> {
            self.entered.send(()).unwrap();
            self.release.recv_timeout(Duration::from_secs(5)).unwrap();
            Ok("first".into())
        }
        fn store(&mut self, _: bool, _: String, _: Instant) -> Result<(), arboard::Error> {
            Ok(())
        }
    }
    let worker = TextWorker::start(move || {
        Ok(Held {
            entered: entered_tx,
            release: blocked,
        })
    })
    .unwrap();
    let first = worker.clone();
    let first =
        std::thread::spawn(move || first.load(false, 10, Instant::now() + worker::READ_TIMEOUT));
    entered.recv_timeout(Duration::from_secs(5)).unwrap();
    assert!(matches!(
        worker.load(true, 10, Instant::now() + Duration::from_millis(20)),
        Err(arboard::Error::Timeout)
    ));
    let started = Instant::now();
    assert!(matches!(
        worker.load(false, 10, Instant::now() + worker::READ_TIMEOUT),
        Err(arboard::Error::Timeout)
    ));
    assert!(started.elapsed() < Duration::from_secs(1));
    release.send(()).unwrap();
    assert_eq!(first.join().unwrap().unwrap(), "first");
}

//! Bounded desktop-notification dispatch.
//!
//! Platform notification services are external IPC. They may block, fail, or
//! panic, so no winit event-loop path calls them directly.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender, TrySendError};
use std::time::{Duration, Instant};

const QUEUE_CAPACITY: usize = 64;

#[derive(Debug)]
struct DesktopNotification {
    title: String,
    body: String,
}

#[derive(Debug)]
enum Message {
    Show(DesktopNotification),
    Flush(SyncSender<()>),
}

struct Dispatcher {
    sender: SyncSender<Message>,
    // Retain the handle for the lifetime of the process. Normal GUI shutdown
    // performs a bounded flush; a stuck platform service must never turn that
    // flush into an unbounded join.
    _worker: std::thread::JoinHandle<()>,
}

static DISPATCHER: OnceLock<Option<Dispatcher>> = OnceLock::new();
static QUEUE_FULL: AtomicBool = AtomicBool::new(false);
static DISCONNECTED: AtomicBool = AtomicBool::new(false);

fn show(notification: DesktopNotification) {
    let mut native = notify_rust::Notification::new();
    native.summary(&notification.title);
    if !notification.body.is_empty() {
        native.body(&notification.body);
    }
    native.appname("kettle");
    if let Err(error) = native.show() {
        log::warn!("kettle.notify: notification send failed: {error}");
    }
}

fn spawn_dispatcher(
    capacity: usize,
    backend: impl Fn(DesktopNotification) + Send + 'static,
) -> std::io::Result<Dispatcher> {
    let (sender, receiver) = mpsc::sync_channel(capacity);
    let worker = std::thread::Builder::new()
        .name("kettle-notifications".to_string())
        .spawn(move || {
            while let Ok(message) = receiver.recv() {
                match message {
                    Message::Show(notification) => {
                        // OS notification backends are outside Kettle's trust
                        // boundary. One panic must not disconnect the
                        // process-wide dispatcher.
                        if std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            backend(notification)
                        }))
                        .is_err()
                        {
                            log::warn!("kettle.notify: notification backend panicked");
                        }
                    }
                    Message::Flush(acknowledge) => {
                        let _ = acknowledge.send(());
                    }
                }
            }
        })?;
    Ok(Dispatcher {
        sender,
        _worker: worker,
    })
}

fn dispatcher() -> Option<&'static Dispatcher> {
    DISPATCHER
        .get_or_init(|| match spawn_dispatcher(QUEUE_CAPACITY, show) {
            Ok(dispatcher) => Some(dispatcher),
            Err(error) => {
                log::warn!("kettle.notify: notification worker could not start: {error}");
                None
            }
        })
        .as_ref()
}

fn try_queue(
    sender: &SyncSender<Message>,
    title: &str,
    body: &str,
) -> Result<(), TrySendError<Message>> {
    sender.try_send(Message::Show(DesktopNotification {
        title: title.to_string(),
        body: body.to_string(),
    }))
}

/// Queue a desktop notification without waiting on the platform service.
///
/// The single bounded worker preserves order. If the platform service stalls,
/// later notifications are dropped instead of freezing rendering, input, or
/// control replies.
pub fn queue_desktop_notification(title: &str, body: &str) {
    let Some(dispatcher) = dispatcher() else {
        return;
    };
    match try_queue(&dispatcher.sender, title, body) {
        Ok(()) => {
            QUEUE_FULL.store(false, Ordering::Release);
        }
        Err(TrySendError::Full(_)) => {
            if !QUEUE_FULL.swap(true, Ordering::AcqRel) {
                log::warn!("kettle.notify: notification queue is full; dropping notifications");
            }
        }
        Err(TrySendError::Disconnected(_)) => {
            if !DISCONNECTED.swap(true, Ordering::AcqRel) {
                log::warn!("kettle.notify: notification worker stopped; dropping notifications");
            }
        }
    }
}

fn flush_sender(sender: &SyncSender<Message>, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    let (acknowledge, acknowledged) = mpsc::sync_channel(0);
    let mut message = Message::Flush(acknowledge);
    loop {
        match sender.try_send(message) {
            Ok(()) => break,
            Err(TrySendError::Full(returned)) => {
                message = returned;
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    return false;
                };
                std::thread::sleep(remaining.min(Duration::from_millis(5)));
            }
            Err(TrySendError::Disconnected(_)) => return false,
        }
    }
    let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
        return false;
    };
    acknowledged.recv_timeout(remaining).is_ok()
}

/// Give already-admitted notifications a short chance to leave during normal
/// GUI shutdown. This is deliberately bounded: a hung platform service must
/// not make Kettle impossible to close.
pub fn flush_desktop_notifications(timeout: Duration) {
    let Some(dispatcher) = DISPATCHER.get().and_then(Option::as_ref) else {
        return;
    };
    if !flush_sender(&dispatcher.sender, timeout) {
        log::debug!("kettle.notify: notification drain did not finish before exit");
    }
}

#[cfg(test)]
mod tests {
    use super::{Dispatcher, Message, flush_sender, spawn_dispatcher, try_queue};
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn admission_is_bounded_and_nonblocking() {
        let (sender, receiver) = mpsc::sync_channel(1);
        try_queue(&sender, "first", "body").unwrap();
        let full = try_queue(&sender, "second", "later")
            .expect_err("a full notification queue must reject immediately");
        let mpsc::TrySendError::Full(Message::Show(rejected)) = full else {
            panic!("a live full queue returned the wrong admission result")
        };
        assert_eq!(rejected.title, "second");
        assert_eq!(rejected.body, "later");
        let Message::Show(admitted) = receiver.recv().unwrap() else {
            panic!("the admitted message changed kind")
        };
        assert_eq!(admitted.title, "first");
        assert_eq!(admitted.body, "body");

        drop(receiver);
        assert!(matches!(
            try_queue(&sender, "third", "gone"),
            Err(mpsc::TrySendError::Disconnected(_))
        ));
    }

    #[test]
    fn flush_waits_for_preceding_messages_without_joining_the_worker() {
        let (sender, receiver) = mpsc::sync_channel(1);
        try_queue(&sender, "first", "body").unwrap();
        let worker = std::thread::spawn(move || {
            let Message::Show(notification) = receiver.recv().unwrap() else {
                panic!("notification must precede the flush")
            };
            assert_eq!(notification.title, "first");
            let Message::Flush(acknowledge) = receiver.recv().unwrap() else {
                panic!("flush message missing")
            };
            acknowledge.send(()).unwrap();
        });
        assert!(flush_sender(&sender, Duration::from_secs(1)));
        drop(sender);
        worker.join().unwrap();
    }

    #[test]
    fn a_full_stalled_queue_keeps_flush_bounded() {
        let (sender, _receiver) = mpsc::sync_channel(1);
        try_queue(&sender, "first", "body").unwrap();
        let started = std::time::Instant::now();
        assert!(!flush_sender(&sender, Duration::from_millis(20)));
        assert!(started.elapsed() < Duration::from_millis(250));
    }

    #[test]
    fn a_stalled_backend_cannot_block_admission_or_bounded_shutdown() {
        let (entered_sender, entered_receiver) = mpsc::sync_channel(0);
        let (release_sender, release_receiver) = mpsc::sync_channel(0);
        let dispatcher = spawn_dispatcher(1, move |notification| {
            if notification.title == "block" {
                entered_sender.send(()).unwrap();
                release_receiver.recv().unwrap();
            }
        })
        .unwrap();

        try_queue(&dispatcher.sender, "block", "backend").unwrap();
        entered_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("the injected backend never started");

        let admission_started = std::time::Instant::now();
        try_queue(&dispatcher.sender, "queued", "later").unwrap();
        assert!(matches!(
            try_queue(&dispatcher.sender, "dropped", "full"),
            Err(mpsc::TrySendError::Full(_))
        ));
        assert!(
            admission_started.elapsed() < Duration::from_millis(250),
            "a blocked backend reached the caller instead of only its worker"
        );

        let shutdown_started = std::time::Instant::now();
        assert!(!flush_sender(&dispatcher.sender, Duration::from_millis(20)));
        assert!(
            shutdown_started.elapsed() < Duration::from_millis(250),
            "bounded shutdown waited for a stalled notification backend"
        );

        release_sender.send(()).unwrap();
        assert!(flush_sender(&dispatcher.sender, Duration::from_secs(1)));
        let Dispatcher {
            sender,
            _worker: worker,
        } = dispatcher;
        drop(sender);
        worker.join().unwrap();
    }
}

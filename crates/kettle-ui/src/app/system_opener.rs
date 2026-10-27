//! Bounded system launches; a slow desktop handler must not block the UI.

use std::ffi::{OsStr, OsString};
use std::io;
use std::sync::Arc;
use std::sync::atomic::AtomicUsize;
use std::thread::JoinHandle;

const MAX_OPENERS: usize = 4;
static OPENERS: AtomicUsize = AtomicUsize::new(0);

/// Report admission errors on the caller and launcher errors on its worker.
pub(super) fn open(target: impl AsRef<OsStr>, report: impl Fn(io::Error) + Send + Sync + 'static) {
    let _ = spawn_with(
        &OPENERS,
        MAX_OPENERS,
        target.as_ref().to_owned(),
        |target| open::that_detached(target),
        report,
    );
}

fn spawn_with(
    counter: &'static AtomicUsize,
    limit: usize,
    target: OsString,
    action: impl FnOnce(&OsStr) -> io::Result<()> + Send + 'static,
    report: impl Fn(io::Error) + Send + Sync + 'static,
) -> Option<JoinHandle<()>> {
    let Some(slot) = super::take_opener_slot(counter, limit) else {
        report(io::Error::new(
            io::ErrorKind::WouldBlock,
            "too many system launches in progress",
        ));
        return None;
    };
    let report = Arc::new(report);
    let worker_report = report.clone();
    match std::thread::Builder::new()
        .name("kettle-system-opener".into())
        .spawn(move || {
            let _slot = slot;
            if let Err(error) = action(&target) {
                worker_report(error);
            }
        }) {
        Ok(handle) => Some(handle),
        Err(error) => {
            report(error);
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::Ordering;
    use std::sync::{Mutex, mpsc};
    use std::time::Duration;

    #[test]
    fn blocked_launcher_leaves_caller_free_and_bounds_concurrency() {
        let counter = Box::leak(Box::new(AtomicUsize::new(0)));
        let counter: &'static AtomicUsize = counter;
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (returned_tx, returned_rx) = mpsc::channel();
        let errors = Arc::new(Mutex::new(Vec::new()));
        let worker_errors = errors.clone();
        let caller = std::thread::spawn(move || {
            let handle = spawn_with(
                counter,
                1,
                "https://example.invalid/".into(),
                move |_| {
                    entered_tx.send(()).unwrap();
                    release_rx.recv().unwrap();
                    Ok(())
                },
                move |error| worker_errors.lock().unwrap().push(error.kind()),
            );
            returned_tx.send(handle).unwrap();
        });
        let entered = entered_rx.recv_timeout(Duration::from_secs(5));
        let returned = returned_rx.recv_timeout(Duration::from_secs(5));
        let caller_was_free = returned.is_ok();
        let busy_errors = errors.clone();
        let refused = spawn_with(
            counter,
            1,
            "must not launch".into(),
            |_| panic!("a busy opener admitted another launch"),
            move |error| busy_errors.lock().unwrap().push(error.kind()),
        );
        // Release and join before assertions, including the synchronous regression.
        let _ = release_tx.send(());
        let handle = returned.unwrap_or_else(|_| returned_rx.recv().unwrap());
        caller.join().unwrap();
        if let Some(handle) = handle {
            handle.join().unwrap();
        }
        assert!(entered.is_ok(), "the launcher never started");
        assert!(caller_was_free, "a blocked launcher held the caller");
        assert!(refused.is_none());
        assert_eq!(*errors.lock().unwrap(), [io::ErrorKind::WouldBlock]);
        assert_eq!(counter.load(Ordering::Acquire), 0);
        spawn_with(
            counter,
            1,
            "next".into(),
            |_| Ok(()),
            |_| panic!("slot leaked"),
        )
        .expect("the completed launch must return its slot")
        .join()
        .unwrap();
    }

    #[test]
    fn launcher_failure_reports_original_error_once_and_releases_slot() {
        let counter = Box::leak(Box::new(AtomicUsize::new(0)));
        let errors = Arc::new(Mutex::new(Vec::new()));
        let reported = errors.clone();
        spawn_with(
            counter,
            1,
            "target".into(),
            |_| Err(io::Error::new(io::ErrorKind::NotFound, "missing handler")),
            move |error| {
                reported
                    .lock()
                    .unwrap()
                    .push((error.kind(), error.to_string()))
            },
        )
        .unwrap()
        .join()
        .unwrap();
        assert_eq!(
            *errors.lock().unwrap(),
            [(io::ErrorKind::NotFound, "missing handler".into())]
        );
        assert_eq!(counter.load(Ordering::Acquire), 0);
    }

    #[cfg(unix)]
    #[test]
    fn launcher_preserves_non_utf8_paths() {
        use std::os::unix::ffi::OsStringExt;
        let counter = Box::leak(Box::new(AtomicUsize::new(0)));
        let target = OsString::from_vec(b"/tmp/kettle-\xff.txt".to_vec());
        let actual = Arc::new(Mutex::new(None));
        let received = actual.clone();
        spawn_with(
            counter,
            1,
            target.clone(),
            move |path| {
                *received.lock().unwrap() = Some(path.to_owned());
                Ok(())
            },
            |_| panic!("launch failed"),
        )
        .unwrap()
        .join()
        .unwrap();
        assert_eq!(*actual.lock().unwrap(), Some(target));
        assert_eq!(counter.load(Ordering::Acquire), 0);
    }
}

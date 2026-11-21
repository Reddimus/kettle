use super::ProfileInventory;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
    mpsc,
};
use std::time::Duration;

pub(crate) struct BlockedInventory {
    pub(crate) inventory: ProfileInventory,
    pub(crate) calls: Arc<AtomicUsize>,
    pub(crate) completed: Arc<AtomicUsize>,
    pub(crate) entered: mpsc::Receiver<usize>,
    pub(crate) release: mpsc::Sender<()>,
    pub(crate) wake: mpsc::Receiver<()>,
}

impl BlockedInventory {
    pub(crate) fn new() -> Self {
        Self::with_results(|call| vec![format!("profile{call}")])
    }

    pub(crate) fn with_results(result: impl Fn(usize) -> Vec<String> + Send + 'static) -> Self {
        Self::with_fallible_results(move |call| Ok(result(call)))
    }

    pub(crate) fn with_fallible_results(
        result: impl Fn(usize) -> std::io::Result<Vec<String>> + Send + 'static,
    ) -> Self {
        let (entered_tx, entered) = mpsc::channel();
        let (release, releases) = mpsc::channel();
        let releases = std::sync::Mutex::new(releases);
        let calls = Arc::new(AtomicUsize::new(0));
        let count = calls.clone();
        let completed = Arc::new(AtomicUsize::new(0));
        let finished = completed.clone();
        let (wakes, wake) = mpsc::channel();
        let inventory = ProfileInventory::with_enumerator(
            move || {
                let call = count.fetch_add(1, Ordering::SeqCst);
                entered_tx.send(call).unwrap();
                let _ = releases
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(5));
                finished.fetch_add(1, Ordering::SeqCst);
                result(call)
            },
            move || {
                let _ = wakes.send(());
            },
        )
        .unwrap();
        Self {
            inventory,
            calls,
            completed,
            entered,
            release,
            wake,
        }
    }

    pub(crate) fn join(mut self) {
        let worker = self.inventory.worker.take().unwrap();
        drop(self);
        worker.join().unwrap();
    }
}

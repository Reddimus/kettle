use std::sync::Mutex;

/// Completion and waker registration share a lock so a late poll cannot miss exit.
#[derive(Debug, Default)]
pub(crate) struct WaiterShared {
    state: Mutex<WaiterState>,
}

#[derive(Debug, Default)]
struct WaiterState {
    waker: Option<std::task::Waker>,
    completed: bool,
}

impl WaiterShared {
    pub(crate) fn store(&self, waker: &std::task::Waker) {
        if let Ok(mut state) = self.state.lock() {
            if state.completed {
                drop(state);
                waker.wake_by_ref();
            } else {
                state.waker = Some(waker.clone());
            }
        }
    }

    pub(crate) fn wake(&self) {
        let waker = self.state.lock().ok().and_then(|mut state| {
            state.completed = true;
            state.waker.take()
        });
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use std::task::{Wake, Waker};

    #[test]
    fn completion_between_status_check_and_waker_replacement_wakes_new_task() {
        let shared = WaiterShared::default();
        let first = Arc::new(CountingWaker(AtomicUsize::new(0)));
        let second = Arc::new(CountingWaker(AtomicUsize::new(0)));
        shared.store(&Waker::from(Arc::clone(&first)));
        // The pending status was checked before the waiter completed.
        shared.wake();
        shared.store(&Waker::from(Arc::clone(&second)));
        assert_eq!(first.0.load(Ordering::Acquire), 1);
        assert_eq!(
            second.0.load(Ordering::Acquire),
            1,
            "late waker missed completion"
        );
    }

    struct CountingWaker(AtomicUsize);

    impl Wake for CountingWaker {
        fn wake(self: Arc<Self>) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }

        fn wake_by_ref(self: &Arc<Self>) {
            self.0.fetch_add(1, Ordering::AcqRel);
        }
    }

    /// An executor may poll with a different waker each time, and only the
    /// latest is valid. Storing must REPLACE, so a stale waker is never the one
    /// woken — and so repeated polls cannot accumulate state per poll, which is
    /// what previously grew a thread population without bound.
    #[test]
    fn storing_a_waker_replaces_the_previous_one() {
        let shared = WaiterShared::default();

        let first = Arc::new(CountingWaker(AtomicUsize::new(0)));
        let second = Arc::new(CountingWaker(AtomicUsize::new(0)));
        shared.store(&Waker::from(Arc::clone(&first)));
        shared.store(&Waker::from(Arc::clone(&second)));

        shared.wake();

        assert_eq!(
            first.0.load(Ordering::Acquire),
            0,
            "a superseded waker must not be woken"
        );
        assert_eq!(
            second.0.load(Ordering::Acquire),
            1,
            "the most recent waker must be the one woken"
        );
    }

    /// The waiter fires once. A second wake with nothing stored must be a
    /// no-op rather than re-waking a consumed waker.
    #[test]
    fn waking_twice_does_not_rewake_a_consumed_waker() {
        let shared = WaiterShared::default();
        let waker = Arc::new(CountingWaker(AtomicUsize::new(0)));
        shared.store(&Waker::from(Arc::clone(&waker)));

        shared.wake();
        shared.wake();

        assert_eq!(
            waker.0.load(Ordering::Acquire),
            1,
            "the waker must be consumed by the first wake"
        );
    }
}

//! Render pipelines compiled on worker threads during renderer setup.
//!
//! The driver's shader compiler is the slow part of a cold renderer setup:
//! Kettle's pipelines took about 63 ms one after another on NVIDIA, with an
//! empty shader cache. Pipelines that depend on nothing else in the setup
//! compile at the same time as the rest of it.

/// A value, normally a render pipeline, being built on its own thread, or
/// waiting to be built where it is joined. [`join`](Self::join) returns what
/// building it there would have.
pub(crate) struct PipelineBuild<T> {
    handle: Option<std::thread::JoinHandle<T>>,
    at_join: Option<Box<dyn FnOnce() -> T>>,
}

impl<T: Send + 'static> PipelineBuild<T> {
    /// [`spawn`](Self::spawn) `build` when `on_thread`; otherwise `build`
    /// runs on this thread when the build is joined.
    pub(crate) fn start<F>(on_thread: bool, name: &'static str, build: F) -> Self
    where
        F: FnOnce() -> T + Clone + Send + 'static,
    {
        if on_thread {
            Self::spawn(name, build)
        } else {
            Self::at_join(build)
        }
    }

    /// Start `build` on a thread named `name`. When no thread can be started,
    /// `build` runs on this thread when the build is joined.
    pub(crate) fn spawn<F>(name: &'static str, build: F) -> Self
    where
        F: FnOnce() -> T + Clone + Send + 'static,
    {
        let here = build.clone();
        #[cfg(test)]
        let refused = tests::threads_refused();
        #[cfg(not(test))]
        let refused = false;
        let spawned = if refused {
            Err(std::io::Error::other("threads refused for a test"))
        } else {
            std::thread::Builder::new().name(name.into()).spawn(build)
        };
        match spawned {
            Ok(handle) => {
                #[cfg(test)]
                tests::note_spawn(name);
                Self {
                    handle: Some(handle),
                    at_join: None,
                }
            }
            Err(error) => {
                log::debug!("building {name} on this thread: {error}");
                Self::at_join(here)
            }
        }
    }

    fn at_join(build: impl FnOnce() -> T + 'static) -> Self {
        Self {
            handle: None,
            at_join: Some(Box::new(build)),
        }
    }

    /// The built value. Where panics unwind, as in tests, a panic in `build`
    /// resumes here, as it would have had `build` run on this thread; the
    /// release profile aborts on it, as it did then.
    pub(crate) fn join(mut self) -> T {
        match self.handle.take() {
            Some(handle) => handle
                .join()
                .unwrap_or_else(|panic| std::panic::resume_unwind(panic)),
            None => (self
                .at_join
                .take()
                .expect("a pipeline build has a thread or a build"))(),
        }
    }
}

impl<T> Drop for PipelineBuild<T> {
    /// A thread dropped before it was joined, when setup fails partway, is
    /// still waited for, so no build outlives the setup that started it. A
    /// build waiting for its join never runs.
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::PipelineBuild;
    use std::cell::RefCell;

    thread_local! {
        /// The builds this thread started on threads of their own, by name,
        /// so a test can see what a constructor it called moved off it.
        static SPAWNED: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
    }

    thread_local! {
        /// Whether this thread's builds are refused a thread, as when the
        /// process has reached its thread limit.
        static REFUSE_THREADS: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }

    pub(super) fn note_spawn(name: &'static str) {
        SPAWNED.with_borrow_mut(|spawned| spawned.push(name));
    }

    pub(super) fn threads_refused() -> bool {
        REFUSE_THREADS.get()
    }

    /// The builds this thread put on threads of their own since the last call.
    pub(crate) fn take_spawned() -> Vec<&'static str> {
        SPAWNED.with_borrow_mut(std::mem::take)
    }

    #[test]
    fn a_build_runs_on_its_own_named_thread() {
        let caller = std::thread::current().id();
        let build = PipelineBuild::spawn("kettle-test-pl", move || {
            let worker = std::thread::current();
            (
                worker.id() != caller,
                worker.name().map(str::to_owned),
                6 * 7,
            )
        });
        assert_eq!(
            build.join(),
            (true, Some("kettle-test-pl".to_owned()), 42),
            "the build ran on its own thread and returned its value"
        );
    }

    #[test]
    fn a_panicking_build_panics_where_it_is_joined() {
        let build = PipelineBuild::<u8>::spawn("kettle-test-pl", || panic!("shader rejected"));
        let panic = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build.join()))
            .expect_err("the join re-raises the build's panic");
        assert_eq!(panic.downcast_ref::<&str>(), Some(&"shader rejected"));
    }

    #[test]
    fn a_build_dropped_unjoined_is_waited_for() {
        let finished = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let done = finished.clone();
        let build = PipelineBuild::spawn("kettle-test-pl", move || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            done.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        drop(build);
        assert!(
            finished.load(std::sync::atomic::Ordering::SeqCst),
            "dropping a build waits for its thread"
        );
    }

    /// A build kept off threads, as on GL, runs on the joining thread when it
    /// is joined, where the pipeline was built before, and not before.
    #[test]
    fn a_build_kept_on_this_thread_runs_where_it_is_joined() {
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mark = ran.clone();
        let build = PipelineBuild::start(false, "kettle-test-pl", move || {
            mark.store(true, std::sync::atomic::Ordering::SeqCst);
            std::thread::current().id()
        });
        assert!(
            !ran.load(std::sync::atomic::Ordering::SeqCst),
            "nothing is built before the join"
        );
        assert_eq!(build.join(), std::thread::current().id());
        assert!(ran.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(take_spawned(), Vec::<&str>::new(), "no thread was started");
    }

    /// A build that cannot get a thread runs on the joining thread when it is
    /// joined, where the pipeline was built before, and not before.
    #[test]
    fn a_build_that_gets_no_thread_runs_where_it_is_joined() {
        REFUSE_THREADS.set(true);
        let ran = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mark = ran.clone();
        let build = PipelineBuild::spawn("kettle-test-pl", move || {
            mark.store(true, std::sync::atomic::Ordering::SeqCst);
            std::thread::current().id()
        });
        REFUSE_THREADS.set(false);
        assert!(
            !ran.load(std::sync::atomic::Ordering::SeqCst),
            "nothing is built before the join"
        );
        assert_eq!(build.join(), std::thread::current().id());
        assert_eq!(take_spawned(), Vec::<&str>::new(), "no thread was started");
    }
}

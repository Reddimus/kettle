//! Bounded worker-owned file persistence for latency-sensitive producers.

use std::io::Write;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError};

use crate::Waker;

/// A single queued write stays small enough that a producer never clones an
/// attacker-controlled PTY burst into one correspondingly large allocation.
pub(crate) const MAX_PERSISTENCE_ITEM_BYTES: usize = 128 * 1024;

/// A short burst can be absorbed without allowing a stalled device to retain
/// an unbounded number of allocation headers and payload vectors.
const DEFAULT_QUEUE_MESSAGES: usize = 128;
/// The byte budget, rather than message count alone, bounds the dominant memory
/// cost when every admitted PTY chunk is near the per-item ceiling.
const DEFAULT_QUEUE_BYTES: usize = 4 * 1024 * 1024;
/// A silent tail must reach durable file APIs promptly, and a worker-side flush
/// failure must become visible even when no later terminal output arrives.
pub(crate) const DEFAULT_FLUSH_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AsyncWriterStatus {
    Active,
    Overloaded,
    IoError,
    Finished,
}

impl AsyncWriterStatus {
    fn encode(self) -> u8 {
        match self {
            Self::Active => 0,
            Self::Overloaded => 1,
            Self::IoError => 2,
            Self::Finished => 3,
        }
    }

    fn decode(value: u8) -> Self {
        match value {
            0 => Self::Active,
            1 => Self::Overloaded,
            2 => Self::IoError,
            _ => Self::Finished,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct PersistenceLimits {
    messages: usize,
    bytes: usize,
    item_bytes: usize,
}

impl Default for PersistenceLimits {
    fn default() -> Self {
        Self {
            messages: DEFAULT_QUEUE_MESSAGES,
            bytes: DEFAULT_QUEUE_BYTES,
            item_bytes: MAX_PERSISTENCE_ITEM_BYTES,
        }
    }
}

#[cfg(test)]
impl PersistenceLimits {
    pub(crate) const fn for_test(messages: usize, bytes: usize, item_bytes: usize) -> Self {
        Self {
            messages,
            bytes,
            item_bytes,
        }
    }
}

struct QueuedWrite {
    bytes: Vec<u8>,
    reserved: usize,
}

/// A file-like sink whose writes, flushes, and final close are owned by one
/// worker. Producers only reserve bounded queue space and use `try_send`.
pub(crate) struct AsyncFileWriter {
    sender: Option<Sender<QueuedWrite>>,
    queued_bytes: Arc<AtomicUsize>,
    limits: PersistenceLimits,
    status: Arc<AtomicU8>,
    notifier: Arc<Mutex<Option<Waker>>>,
    worker: Option<JoinHandle<()>>,
    finish_requested: bool,
}

impl AsyncFileWriter {
    pub(crate) fn spawn(thread_name: &str, writer: Box<dyn Write + Send>) -> std::io::Result<Self> {
        Self::spawn_with_limits(thread_name, writer, PersistenceLimits::default())
    }

    pub(crate) fn spawn_with_limits(
        thread_name: &str,
        writer: Box<dyn Write + Send>,
        limits: PersistenceLimits,
    ) -> std::io::Result<Self> {
        if limits.messages == 0 || limits.bytes == 0 || limits.item_bytes == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "persistence queue limits must be nonzero",
            ));
        }
        let (sender, receiver) = crossbeam_channel::bounded(limits.messages);
        let queued_bytes = Arc::new(AtomicUsize::new(0));
        let status = Arc::new(AtomicU8::new(AsyncWriterStatus::Active.encode()));
        let notifier = Arc::new(Mutex::new(None::<Waker>));
        let worker_queued_bytes = Arc::clone(&queued_bytes);
        let worker_status = Arc::clone(&status);
        let worker_notifier = Arc::clone(&notifier);
        let worker = std::thread::Builder::new()
            .name(thread_name.into())
            .spawn(move || {
                run_writer(
                    writer,
                    receiver,
                    &worker_queued_bytes,
                    &worker_status,
                    &worker_notifier,
                );
            })?;
        Ok(Self {
            sender: Some(sender),
            queued_bytes,
            limits,
            status,
            notifier,
            worker: Some(worker),
            finish_requested: false,
        })
    }

    pub(crate) fn set_failure_waker(&mut self, waker: Waker) {
        if let Ok(mut notifier) = self.notifier.lock() {
            *notifier = Some(waker);
        }
    }

    pub(crate) fn status(&self) -> AsyncWriterStatus {
        AsyncWriterStatus::decode(self.status.load(Ordering::Acquire))
    }

    pub(crate) fn try_write(&mut self, bytes: Vec<u8>) -> Result<(), AsyncWriterStatus> {
        if bytes.is_empty() {
            return Ok(());
        }
        if self.status() != AsyncWriterStatus::Active || self.finish_requested {
            return Err(self.status());
        }
        let reserved = bytes.len();
        if reserved > self.limits.item_bytes || !self.reserve(reserved) {
            self.stop_with(AsyncWriterStatus::Overloaded);
            return Err(AsyncWriterStatus::Overloaded);
        }
        let item = QueuedWrite { bytes, reserved };
        let result = self
            .sender
            .as_ref()
            .expect("active persistence writer must retain its sender")
            .try_send(item);
        match result {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(item)) => {
                self.release(item.reserved);
                self.stop_with(AsyncWriterStatus::Overloaded);
                Err(AsyncWriterStatus::Overloaded)
            }
            Err(TrySendError::Disconnected(item)) => {
                self.release(item.reserved);
                self.stop_with(AsyncWriterStatus::IoError);
                Err(AsyncWriterStatus::IoError)
            }
        }
    }

    fn reserve(&self, bytes: usize) -> bool {
        let mut current = self.queued_bytes.load(Ordering::Acquire);
        loop {
            let Some(next) = current.checked_add(bytes) else {
                return false;
            };
            if next > self.limits.bytes {
                return false;
            }
            match self.queued_bytes.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    fn release(&self, bytes: usize) {
        let previous = self.queued_bytes.fetch_sub(bytes, Ordering::AcqRel);
        debug_assert!(previous >= bytes, "persistence byte reservation underflow");
    }

    fn stop_with(&mut self, failure: AsyncWriterStatus) {
        set_failure(&self.status, &self.notifier, failure);
        // Closing the producer end lets the worker drain every write that was
        // already accepted. Keeping it open after overload would leave a
        // permanently idle worker and an audit artifact with no close flush.
        self.request_finish();
    }

    /// Recorder-only: the session log has no overload/finish protocol of
    /// its own. Gated exactly like `mod record`, which is also built under
    /// `cfg(test)` with the feature off, so this is absent rather than dead
    /// code in a plain build.
    #[cfg(any(feature = "asciicast", test))]
    pub(crate) fn stop_overloaded(&mut self) {
        self.stop_with(AsyncWriterStatus::Overloaded);
    }

    pub(crate) fn request_finish(&mut self) {
        self.finish_requested = true;
        drop(self.sender.take());
    }

    /// Recorder-only: the session log has no overload/finish protocol of
    /// its own. Gated exactly like `mod record`, which is also built under
    /// `cfg(test)` with the feature off, so this is absent rather than dead
    /// code in a plain build.
    #[cfg(any(feature = "asciicast", test))]
    pub(crate) fn finish_requested(&self) -> bool {
        self.finish_requested
    }

    pub(crate) fn try_join(&mut self) -> bool {
        let Some(worker) = self.worker.as_ref() else {
            return true;
        };
        if !worker.is_finished() {
            return false;
        }
        if self
            .worker
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            set_failure(&self.status, &self.notifier, AsyncWriterStatus::IoError);
        }
        true
    }

    /// Recorder-only: the session log has no overload/finish protocol of
    /// its own. Gated exactly like `mod record`, which is also built under
    /// `cfg(test)` with the feature off, so this is absent rather than dead
    /// code in a plain build.
    #[cfg(any(feature = "asciicast", test))]
    pub(crate) fn finish_with_timeout(&mut self, timeout: Duration) -> bool {
        self.request_finish();
        let deadline = Instant::now() + timeout;
        loop {
            if self.try_join() {
                return self.status() != AsyncWriterStatus::IoError;
            }
            if Instant::now() >= deadline {
                // A write that does not return is operationally indistinguishable
                // from an I/O failure to the caller. Detach it so the bounded wait
                // cannot turn back into the original liveness failure.
                set_failure(&self.status, &self.notifier, AsyncWriterStatus::IoError);
                drop(self.worker.take());
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

impl Drop for AsyncFileWriter {
    fn drop(&mut self) {
        self.request_finish();
        if !self.try_join() {
            // Dropping a JoinHandle detaches the worker. The sink stays owned by
            // that worker, so even close-time filesystem latency cannot leak
            // back onto the producer that is tearing down.
            drop(self.worker.take());
        }
    }
}

fn run_writer(
    mut writer: Box<dyn Write + Send>,
    receiver: Receiver<QueuedWrite>,
    queued_bytes: &AtomicUsize,
    status: &AtomicU8,
    notifier: &Mutex<Option<Waker>>,
) {
    let mut dirty = false;
    let mut flush_deadline = Instant::now() + DEFAULT_FLUSH_INTERVAL;
    loop {
        // Check the deadline here rather than relying on `recv_timeout` to
        // report it. Once the deadline has passed the computed timeout is
        // zero, and a zero timeout does NOT yield `Timeout` while an item is
        // ready — it yields the item. So a continuously writing producer
        // starved the flush arm completely: buffered data sat past the bound
        // until the stream went idle, and a flush failure (a full disk, say)
        // stayed invisible for exactly as long, which is the opposite of what
        // the visible-failure design is for.
        if dirty && Instant::now() >= flush_deadline {
            if writer.flush().is_err() {
                set_failure(status, notifier, AsyncWriterStatus::IoError);
                return;
            }
            dirty = false;
        }
        let received = if dirty {
            receiver.recv_timeout(flush_deadline.saturating_duration_since(Instant::now()))
        } else {
            match receiver.recv() {
                Ok(item) => Ok(item),
                Err(_) => Err(RecvTimeoutError::Disconnected),
            }
        };
        match received {
            Ok(item) => {
                // Once dequeued, this allocation is bounded independently by
                // the item ceiling. Releasing its queue reservation here lets
                // producers use the advertised byte budget while the worker
                // owns at most one additional bounded write.
                let previous = queued_bytes.fetch_sub(item.reserved, Ordering::AcqRel);
                debug_assert!(
                    previous >= item.reserved,
                    "persistence worker byte reservation underflow"
                );
                if writer.write_all(&item.bytes).is_err() {
                    set_failure(status, notifier, AsyncWriterStatus::IoError);
                    return;
                }
                if !dirty {
                    flush_deadline = Instant::now() + DEFAULT_FLUSH_INTERVAL;
                }
                dirty = true;
            }
            Err(RecvTimeoutError::Timeout) => {
                if writer.flush().is_err() {
                    set_failure(status, notifier, AsyncWriterStatus::IoError);
                    return;
                }
                dirty = false;
            }
            Err(RecvTimeoutError::Disconnected) => {
                if writer.flush().is_err() {
                    set_failure(status, notifier, AsyncWriterStatus::IoError);
                    return;
                }
                // Preserve overload and I/O states through the final drain so
                // an owner cannot mistake an incomplete artifact for success.
                let _ = status.compare_exchange(
                    AsyncWriterStatus::Active.encode(),
                    AsyncWriterStatus::Finished.encode(),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                );
                return;
            }
        }
    }
}

fn set_failure(status: &AtomicU8, notifier: &Mutex<Option<Waker>>, failure: AsyncWriterStatus) {
    debug_assert!(matches!(
        failure,
        AsyncWriterStatus::Overloaded | AsyncWriterStatus::IoError
    ));
    let target = failure.encode();
    let changed = if failure == AsyncWriterStatus::IoError {
        // A queue overload can be followed by a flush failure while draining
        // accepted writes. The later I/O error is the stronger final reason.
        status.swap(target, Ordering::AcqRel) != target
    } else {
        status
            .compare_exchange(
                AsyncWriterStatus::Active.encode(),
                target,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    };
    if changed
        && let Ok(notifier) = notifier.lock()
        && let Some(wake) = notifier.as_ref()
    {
        wake();
    }
}

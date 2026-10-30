use std::sync::atomic::{AtomicU8, Ordering};

const FREE: u8 = 0;
const TAKEN: u8 = 1;
const QUEUED: u8 = 2;

pub(crate) struct Buffer(AtomicU8);

impl Buffer {
    pub const fn new() -> Self {
        Self(AtomicU8::new(FREE))
    }

    pub fn take(&self) -> bool {
        self.0
            .compare_exchange(FREE, TAKEN, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn enqueue(&self) -> bool {
        self.0
            .compare_exchange(TAKEN, QUEUED, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }

    pub fn release(&self) {
        self.0.store(FREE, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_buffer_can_only_be_enqueued_once_per_take() {
        let buffer = Buffer::new();
        assert!(buffer.take());
        assert!(buffer.enqueue());
        assert!(
            !buffer.enqueue(),
            "a queued buffer must reject a second enqueue"
        );
        assert!(!buffer.take());
        buffer.release();
        assert!(
            !buffer.enqueue(),
            "a free buffer must be taken before enqueue"
        );
        assert!(buffer.take());
        assert!(buffer.enqueue());
    }
}

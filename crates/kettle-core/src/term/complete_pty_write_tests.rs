use super::write_all_with_backpressure;
use std::collections::VecDeque;
use std::io::{self, Write};

enum Step {
    Accept(usize),
    Zero,
    WouldBlock,
    Interrupted,
}

struct ScriptedWriter {
    steps: VecDeque<Step>,
    written: Vec<u8>,
    flush_would_block: usize,
}

impl ScriptedWriter {
    fn new(steps: impl IntoIterator<Item = Step>) -> Self {
        Self {
            steps: steps.into_iter().collect(),
            written: Vec::new(),
            flush_would_block: 0,
        }
    }
}

impl Write for ScriptedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self.steps.pop_front().unwrap_or(Step::Accept(bytes.len())) {
            Step::Accept(limit) => {
                let accepted = limit.min(bytes.len());
                self.written.extend_from_slice(&bytes[..accepted]);
                Ok(accepted)
            }
            Step::Zero => Ok(0),
            Step::WouldBlock => Err(io::Error::from(io::ErrorKind::WouldBlock)),
            Step::Interrupted => Err(io::Error::from(io::ErrorKind::Interrupted)),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.flush_would_block == 0 {
            Ok(())
        } else {
            self.flush_would_block -= 1;
            Err(io::Error::from(io::ErrorKind::WouldBlock))
        }
    }
}

#[test]
fn retries_zero_would_block_interruption_and_partial_progress_without_loss() {
    let mut writer = ScriptedWriter::new([
        Step::Zero,
        Step::WouldBlock,
        Step::Interrupted,
        Step::Accept(2),
        Step::WouldBlock,
        Step::Accept(usize::MAX),
    ]);
    writer.flush_would_block = 1;
    let mut waits = 0;

    write_all_with_backpressure(&mut writer, b"abcdef", 4, 3, || waits += 1)
        .expect("transient backpressure must recover");

    assert_eq!(writer.written, b"abcdef");
    assert_eq!(
        waits, 4,
        "zero, two WouldBlock writes, and one blocked flush wait"
    );
}

#[test]
fn persistent_zero_progress_is_would_block_with_an_actionable_prefix_count() {
    let mut writer = ScriptedWriter::new([Step::Accept(2), Step::Zero, Step::Zero, Step::Zero]);
    let error = write_all_with_backpressure(&mut writer, b"abcd", 4, 2, || {})
        .expect_err("bounded retry exhaustion must be reported");

    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    assert!(
        error.to_string().contains("2 of 4 bytes delivered"),
        "partial progress must be explicit: {error}"
    );
    assert_eq!(writer.written, b"ab");
    assert_ne!(error.kind(), io::ErrorKind::WriteZero);
}

#[test]
fn successful_progress_resets_the_consecutive_backpressure_bound() {
    let mut writer = ScriptedWriter::new([
        Step::Zero,
        Step::Zero,
        Step::Accept(1),
        Step::WouldBlock,
        Step::WouldBlock,
        Step::Accept(1),
    ]);

    write_all_with_backpressure(&mut writer, b"ab", 1, 2, || {})
        .expect("progress between stalls resets the bound");
    assert_eq!(writer.written, b"ab");
}

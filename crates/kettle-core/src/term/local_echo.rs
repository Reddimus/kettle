//! Bounded admission and acknowledgment for the input worker's local echo.

use super::{LocalOutput, PtyReadProgressState};
use std::io;
use std::sync::Arc;

pub(super) enum LocalPayload {
    Output(Vec<u8>),
    Echo(Arc<[u8]>, crossbeam_channel::Sender<()>),
}

/// A bounded local echo attached to an accepted PTY input message. The input
/// worker polls this before writing, so a child cannot reply ahead of its echo.
/// A rejected queue message drops this without ever changing the terminal.
pub struct LocalEcho {
    bytes: Option<Arc<[u8]>>,
    sender: crossbeam_channel::Sender<LocalOutput>,
    progress: Arc<PtyReadProgressState>,
    ack_sender: Option<crossbeam_channel::Sender<()>>,
    ack: crossbeam_channel::Receiver<()>,
    ready: bool,
}

impl LocalEcho {
    pub(super) fn new(
        bytes: Arc<[u8]>,
        sender: crossbeam_channel::Sender<LocalOutput>,
        progress: Arc<PtyReadProgressState>,
    ) -> Self {
        let (ack_sender, ack) = crossbeam_channel::bounded(1);
        Self {
            bytes: Some(bytes),
            sender,
            progress,
            ack_sender: Some(ack_sender),
            ack,
            ready: false,
        }
    }

    /// `false` is transient backpressure. A stopped reader is an input failure.
    pub fn poll_ready(&mut self) -> io::Result<bool> {
        if self.ready {
            return Ok(true);
        }
        if let Some(bytes) = self.bytes.take() {
            let payload = LocalPayload::Echo(bytes, self.ack_sender.take().expect("not submitted"));
            let generation = self.progress.load().generation;
            match self.sender.try_send((generation, payload)) {
                Ok(()) => {}
                Err(crossbeam_channel::TrySendError::Full((_, LocalPayload::Echo(bytes, ack)))) => {
                    self.bytes = Some(bytes);
                    self.ack_sender = Some(ack);
                    return Ok(false);
                }
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::BrokenPipe,
                        "local echo reader stopped",
                    ));
                }
                Err(crossbeam_channel::TrySendError::Full(_)) => unreachable!(),
            }
        }
        match self.ack.try_recv() {
            Ok(()) => {
                self.ready = true;
                Ok(true)
            }
            Err(crossbeam_channel::TryRecvError::Empty) => Ok(false),
            Err(crossbeam_channel::TryRecvError::Disconnected) => Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "local echo was not parsed",
            )),
        }
    }
}

#[cfg(test)]
mod echo_admission_tests {
    use super::*;

    fn ticket(sender: crossbeam_channel::Sender<LocalOutput>) -> LocalEcho {
        LocalEcho::new(
            Arc::from(&b"echo"[..]),
            sender,
            Arc::new(PtyReadProgressState::new()),
        )
    }

    #[test]
    fn echo_waits_for_capacity_and_a_parser_ack_without_duplicate_submission() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        tx.send((0, LocalPayload::Output(Vec::new())))
            .unwrap_or_else(|_| panic!("fixture"));
        let mut echo = ticket(tx);
        assert!(!echo.poll_ready().unwrap());
        assert!(echo.bytes.is_some());
        drop(rx.recv().unwrap());
        assert!(!echo.poll_ready().unwrap());
        assert!(echo.bytes.is_none());
        let (_, LocalPayload::Echo(bytes, ack)) = rx.recv().unwrap() else {
            panic!("echo")
        };
        assert_eq!(bytes.as_ref(), b"echo");
        assert!(!echo.poll_ready().unwrap());
        assert!(rx.is_empty());
        ack.send(()).unwrap();
        assert!(echo.poll_ready().unwrap());
        assert!(echo.poll_ready().unwrap());
    }

    #[test]
    fn stopped_or_discarded_echo_cannot_let_the_input_worker_write() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let mut echo = ticket(tx);
        assert!(!echo.poll_ready().unwrap());
        drop(rx.recv().unwrap());
        assert_eq!(
            echo.poll_ready().unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
        let (tx, rx) = crossbeam_channel::bounded(1);
        drop(rx);
        assert_eq!(
            ticket(tx).poll_ready().unwrap_err().kind(),
            io::ErrorKind::BrokenPipe
        );
    }
}

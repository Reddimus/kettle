//! One serialized, bounded worker for blocking native selection operations.
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};

use super::{LateAnswer, PasteContent, PasteWant, UserRead};

type Result<T> = std::result::Result<T, arboard::Error>;

pub(super) const READ_TIMEOUT: Duration = Duration::from_secs(2);

/// Whether the provider serves PRIMARY now. It is asked on the UI thread at
/// each use, not once: a compositor announces the primary-selection protocol
/// after the worker has started, and the bridge can close.
pub(super) type PrimaryProbe = Arc<dyn Fn() -> bool + Send + Sync>;

pub(super) trait TextProvider: Send + 'static {
    fn primary_probe(&self) -> PrimaryProbe {
        Arc::new(|| true)
    }
    fn load(&mut self, primary: bool, max: usize, deadline: Instant) -> Result<String>;
    fn store(&mut self, primary: bool, text: String, deadline: Instant) -> Result<()>;
    /// What a paste takes from CLIPBOARD. A provider of text alone gives
    /// the text.
    fn paste(&mut self, _want: PasteWant, text_max: usize, deadline: Instant) -> PasteContent {
        PasteContent::Text(self.load(false, text_max, deadline))
    }
    /// The local files CLIPBOARD names.
    fn files(&mut self, _deadline: Instant) -> Option<Vec<PathBuf>> {
        None
    }
}

enum Operation {
    Load { primary: bool, max: usize },
    Store { primary: bool, text: String },
    Paste { want: PasteWant, text_max: usize },
    Files,
}

enum Reply {
    Text(String),
    Stored,
    Paste(PasteContent),
    Files(Option<Vec<PathBuf>>),
}

struct Request {
    operation: Operation,
    deadline: Instant,
    /// Hands the caller its reply, in the form the caller asked for.
    reply: Box<dyn FnOnce(Result<Reply>) + Send>,
    /// Gone once nobody waits for the reply: a request still queued then is
    /// not served.
    wanted: std::sync::Weak<()>,
}

#[derive(Clone)]
pub(super) struct TextWorker {
    requests: Sender<Request>,
    primary: PrimaryProbe,
}

impl TextWorker {
    pub(super) fn start<P: TextProvider>(
        create: impl FnOnce() -> Result<P> + Send + 'static,
    ) -> Result<Self> {
        let (requests, incoming) = bounded::<Request>(1);
        let (ready, initialized) = bounded(1);
        std::thread::Builder::new()
            .name("kettle-clipboard".into())
            .spawn(move || {
                let mut provider = match create() {
                    Ok(provider) => provider,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                if ready.send(Ok(provider.primary_probe())).is_err() {
                    return;
                }
                for request in incoming {
                    let deadline = request.deadline;
                    let result = if Instant::now() >= deadline || request.wanted.strong_count() == 0
                    {
                        Err(arboard::Error::Timeout)
                    } else {
                        match request.operation {
                            Operation::Load { primary, max } => {
                                provider.load(primary, max, deadline).and_then(|text| {
                                    if text.len() > max {
                                        Err(arboard::Error::LimitExceeded)
                                    } else {
                                        Ok(Reply::Text(text))
                                    }
                                })
                            }
                            Operation::Store { primary, text } => provider
                                .store(primary, text, deadline)
                                .map(|()| Reply::Stored),
                            Operation::Paste { want, text_max } => {
                                Ok(Reply::Paste(provider.paste(want, text_max, deadline)))
                            }
                            Operation::Files => Ok(Reply::Files(provider.files(deadline))),
                        }
                    };
                    // What a read brings after its deadline is dropped here:
                    // the caller has stopped waiting for it.
                    let late = !matches!(result, Ok(Reply::Stored)) && Instant::now() >= deadline;
                    (request.reply)(if late {
                        Err(arboard::Error::Timeout)
                    } else {
                        result
                    });
                }
            })
            .map_err(|_| arboard::Error::ClipboardNotSupported)?;
        let primary = initialized
            .recv_timeout(READ_TIMEOUT)
            .map_err(|_| arboard::Error::Timeout)??;
        Ok(Self { requests, primary })
    }

    /// Queues `operation` and returns where its reply comes, made into a `T`
    /// by `answer`, with the caller's interest in it. The worker drops a reply
    /// that comes after `deadline`, and does not serve the request once the
    /// interest is dropped.
    fn submit<T: Send + 'static>(
        &self,
        operation: Operation,
        deadline: Instant,
        answer: fn(Result<Reply>) -> T,
    ) -> Result<(Receiver<T>, Arc<()>)> {
        let (sender, result): (Sender<T>, _) = bounded(1);
        let interest = Arc::new(());
        self.requests
            .try_send(Request {
                operation,
                deadline,
                reply: Box::new(move |reply| {
                    let _ = sender.send(answer(reply));
                }),
                wanted: Arc::downgrade(&interest),
            })
            .map_err(|error| match error {
                crossbeam_channel::TrySendError::Full(_) => arboard::Error::Timeout,
                crossbeam_channel::TrySendError::Disconnected(_) => {
                    arboard::Error::ClipboardNotSupported
                }
            })?;
        Ok((result, interest))
    }

    fn request(&self, operation: Operation, deadline: Instant) -> Result<Reply> {
        let (result, _interest) = self.submit(operation, deadline, |reply| reply)?;
        result
            .recv_deadline(deadline)
            .map_err(|_| arboard::Error::Timeout)?
    }

    /// Asks for `operation`, waits for its answer until `wait`, and leaves
    /// the rest, up to `deadline`, to come later. `timed_out` is the answer
    /// once `deadline` has passed.
    fn ask<T: Send + 'static>(
        &self,
        operation: Operation,
        (wait, deadline): (Instant, Instant),
        answer: fn(Result<Reply>) -> T,
        timed_out: fn() -> T,
    ) -> UserRead<T> {
        let (result, interest) = match self.submit(operation, deadline, answer) {
            Ok(submitted) => submitted,
            Err(error) => return UserRead::Now(answer(Err(error))),
        };
        match result.recv_deadline(wait.min(deadline)) {
            Ok(answered) => UserRead::Now(answered),
            Err(RecvTimeoutError::Timeout) if wait < deadline => {
                UserRead::Later(LateAnswer::new(result, deadline, timed_out, interest))
            }
            Err(_) => UserRead::Now(timed_out()),
        }
    }

    /// The requests waiting behind the one being served.
    #[cfg(test)]
    pub(super) fn queued(&self) -> usize {
        self.requests.len()
    }

    pub(super) fn supports_primary(&self) -> bool {
        (self.primary)()
    }

    pub(super) fn load(&self, primary: bool, max: usize, deadline: Instant) -> Result<String> {
        text_reply(self.request(Operation::Load { primary, max }, deadline))
    }

    /// [`Self::load`] of CLIPBOARD for a read the user asked for: the caller
    /// waits until `wait`, and an answer that comes after it, by `deadline`,
    /// is still the caller's.
    pub(super) fn load_later(
        &self,
        max: usize,
        wait: Instant,
        deadline: Instant,
    ) -> UserRead<Result<String>> {
        self.ask(
            Operation::Load {
                primary: false,
                max,
            },
            (wait, deadline),
            text_reply,
            || Err(arboard::Error::Timeout),
        )
    }

    pub(super) fn store(&self, primary: bool, text: String) -> Result<()> {
        self.request(
            Operation::Store { primary, text },
            Instant::now() + READ_TIMEOUT,
        )
        .map(|_| ())
    }

    /// What a paste takes from CLIPBOARD. A worker that is busy, gone or
    /// late is a text that could not be read.
    pub(super) fn paste(
        &self,
        want: PasteWant,
        text_max: usize,
        deadline: Instant,
    ) -> PasteContent {
        paste_reply(self.request(Operation::Paste { want, text_max }, deadline))
    }

    /// [`Self::paste`] for a paste the user asked for, waited for as
    /// [`Self::load_later`] waits.
    pub(super) fn paste_later(
        &self,
        want: PasteWant,
        text_max: usize,
        wait: Instant,
        deadline: Instant,
    ) -> UserRead<PasteContent> {
        self.ask(
            Operation::Paste { want, text_max },
            (wait, deadline),
            paste_reply,
            || PasteContent::Text(Err(arboard::Error::Timeout)),
        )
    }

    pub(super) fn files(&self, deadline: Instant) -> Result<Vec<PathBuf>> {
        match self.request(Operation::Files, deadline)? {
            Reply::Files(Some(paths)) => Ok(paths),
            _ => Err(arboard::Error::ContentNotAvailable),
        }
    }
}

fn text_reply(reply: Result<Reply>) -> Result<String> {
    match reply? {
        Reply::Text(text) => Ok(text),
        _ => Err(arboard::Error::ContentNotAvailable),
    }
}

/// A worker that is busy, gone or late is a text that could not be read.
fn paste_reply(reply: Result<Reply>) -> PasteContent {
    match reply {
        Ok(Reply::Paste(content)) => content,
        Ok(_) => PasteContent::Text(Err(arboard::Error::ContentNotAvailable)),
        Err(error) => PasteContent::Text(Err(error)),
    }
}

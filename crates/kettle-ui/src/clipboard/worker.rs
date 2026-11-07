//! One serialized, bounded worker for blocking native text operations.
use std::time::{Duration, Instant};

use crossbeam_channel::{Sender, bounded};

type Result<T> = std::result::Result<T, arboard::Error>;

pub(super) const READ_TIMEOUT: Duration = Duration::from_secs(2);

pub(super) trait TextProvider: Send + 'static {
    fn supports_primary(&self) -> bool {
        true
    }
    fn load(&mut self, primary: bool, max: usize, deadline: Instant) -> Result<String>;
    fn store(&mut self, primary: bool, text: String, deadline: Instant) -> Result<()>;
}

enum Operation {
    Load { primary: bool, max: usize },
    Store { primary: bool, text: String },
}

struct Request {
    operation: Operation,
    deadline: Instant,
    reply: Sender<Result<String>>,
}

#[derive(Clone)]
pub(super) struct TextWorker {
    requests: Sender<Request>,
    primary: bool,
}

impl TextWorker {
    pub(super) fn start<P: TextProvider>(
        create: impl FnOnce() -> Result<P> + Send + 'static,
    ) -> Result<Self> {
        let (requests, incoming) = bounded::<Request>(1);
        let (ready, initialized) = bounded(1);
        std::thread::Builder::new()
            .name("kettle-wayland-clipboard".into())
            .spawn(move || {
                let mut provider = match create() {
                    Ok(provider) => provider,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                if ready.send(Ok(provider.supports_primary())).is_err() {
                    return;
                }
                for request in incoming {
                    let result = if Instant::now() >= request.deadline {
                        Err(arboard::Error::Timeout)
                    } else {
                        match request.operation {
                            Operation::Load { primary, max } => provider
                                .load(primary, max, request.deadline)
                                .and_then(|text| {
                                    if Instant::now() >= request.deadline {
                                        Err(arboard::Error::Timeout)
                                    } else if text.len() > max {
                                        Err(arboard::Error::LimitExceeded)
                                    } else {
                                        Ok(text)
                                    }
                                }),
                            Operation::Store { primary, text } => provider
                                .store(primary, text, request.deadline)
                                .map(|()| String::new()),
                        }
                    };
                    let _ = request.reply.send(result);
                }
            })
            .map_err(|_| arboard::Error::ClipboardNotSupported)?;
        let primary = initialized
            .recv_timeout(READ_TIMEOUT)
            .map_err(|_| arboard::Error::Timeout)??;
        Ok(Self { requests, primary })
    }

    fn request(&self, operation: Operation, deadline: Instant) -> Result<String> {
        let (reply, result) = bounded(1);
        self.requests
            .try_send(Request {
                operation,
                deadline,
                reply,
            })
            .map_err(|error| match error {
                crossbeam_channel::TrySendError::Full(_) => arboard::Error::Timeout,
                crossbeam_channel::TrySendError::Disconnected(_) => {
                    arboard::Error::ClipboardNotSupported
                }
            })?;
        result
            .recv_deadline(deadline)
            .map_err(|_| arboard::Error::Timeout)?
    }

    pub(super) fn supports_primary(&self) -> bool {
        self.primary
    }

    pub(super) fn load(&self, primary: bool, max: usize, deadline: Instant) -> Result<String> {
        self.request(Operation::Load { primary, max }, deadline)
    }

    pub(super) fn store(&self, primary: bool, text: String) -> Result<()> {
        self.request(
            Operation::Store { primary, text },
            Instant::now() + READ_TIMEOUT,
        )
        .map(|_| ())
    }
}

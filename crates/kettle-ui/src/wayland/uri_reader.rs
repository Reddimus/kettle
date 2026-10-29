use std::io::{self, PipeReader, PipeWriter, Read};
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

const READ_WITHIN: Duration = Duration::from_secs(2);
pub(super) type Result = std::result::Result<Vec<PathBuf>, ()>;

pub(super) struct Job {
    stop: Arc<AtomicBool>,
    finished: Arc<AtomicBool>,
    thread: Option<JoinHandle<Result>>,
    deadline: Instant,
}

impl Job {
    pub(super) fn new(wake: kettle_core::Waker) -> io::Result<(Self, PipeWriter)> {
        let (reader, writer) = io::pipe()?;
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let finished = Arc::new(AtomicBool::new(false));
        let done = finished.clone();
        let deadline = Instant::now() + READ_WITHIN;
        let thread = std::thread::Builder::new()
            .name("kettle-drop".into())
            .spawn(move || {
                let result = read(reader, &stopped, deadline);
                done.store(true, Ordering::Release);
                wake();
                result
            })?;
        Ok((
            Self {
                stop,
                finished,
                thread: Some(thread),
                deadline,
            },
            writer,
        ))
    }

    pub(super) fn ready(&mut self) -> Option<Result> {
        if !self.thread.as_ref()?.is_finished() {
            return None;
        }
        Some(self.thread.take()?.join().unwrap_or(Err(())))
    }

    pub(super) fn deadline(&self) -> Instant {
        // The wake precedes thread return; retry that small join race only
        // while a real drop is pending, never as an idle polling loop.
        let now = Instant::now();
        if self.finished.load(Ordering::Acquire) || now >= self.deadline {
            now + Duration::from_millis(1)
        } else {
            self.deadline
        }
    }
}

impl Drop for Job {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn read(mut reader: PipeReader, stop: &AtomicBool, deadline: Instant) -> Result {
    let fd = reader.as_raw_fd();
    // Only the read end is nonblocking; producers keep normal pipe writes.
    // SAFETY: reader owns this live descriptor for the entire operation.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    // SAFETY: the same live descriptor and the flags returned above.
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(());
    }
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    loop {
        let now = Instant::now();
        if stop.load(Ordering::Acquire) || now >= deadline {
            return Err(());
        }
        match reader.read(&mut buffer) {
            Ok(0) => return super::uri_list::parse(&bytes),
            Ok(n) => {
                if n > super::uri_list::MAX_BYTES - bytes.len() {
                    return Err(());
                }
                bytes.extend_from_slice(&buffer[..n]);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                let mut poll = libc::pollfd {
                    fd,
                    events: libc::POLLIN,
                    revents: 0,
                };
                let timeout = deadline
                    .saturating_duration_since(now)
                    .as_millis()
                    .clamp(1, 25);
                // SAFETY: one initialized pollfd and a bounded timeout.
                if unsafe { libc::poll(&mut poll, 1, timeout as i32) } < 0
                    && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted
                {
                    return Err(());
                }
            }
            Err(_) => return Err(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn a_complete_pipe_delivers_paths_and_a_stalled_source_is_cancellable() {
        let (mut job, mut writer) = Job::new(Arc::new(|| {})).unwrap();
        assert_eq!(
            // SAFETY: writer owns the live descriptor being queried; F_GETFL takes no extra
            // argument.
            unsafe { libc::fcntl(writer.as_raw_fd(), libc::F_GETFL) } & libc::O_NONBLOCK,
            0
        );
        writer.write_all(b"file:///tmp/a%20b.png\r\n").unwrap();
        drop(writer);
        assert!(job.deadline() > Instant::now());
        let until = Instant::now() + Duration::from_secs(1);
        let result = loop {
            if let Some(result) = job.ready() {
                break result;
            }
            assert!(Instant::now() < until);
            std::thread::sleep(Duration::from_millis(1));
        };
        assert_eq!(result.unwrap(), [PathBuf::from("/tmp/a b.png")]);
        let (job, _open_writer) = Job::new(Arc::new(|| {})).unwrap();
        let start = Instant::now();
        drop(job);
        assert!(start.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn a_stalled_pipe_has_an_absolute_deadline() {
        let (reader, _writer) = io::pipe().unwrap();
        let start = Instant::now();
        assert!(
            read(
                reader,
                &AtomicBool::new(false),
                start + Duration::from_millis(60)
            )
            .is_err()
        );
        assert!(start.elapsed() >= Duration::from_millis(60));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn an_oversize_producer_is_closed_before_its_open_writer_times_out() {
        let (mut job, mut writer) = Job::new(Arc::new(|| {})).unwrap();
        let (release, held) = std::sync::mpsc::channel();
        let producer = std::thread::spawn(move || {
            let _ = writer.write_all(&vec![b'#'; super::super::uri_list::MAX_BYTES + 1]);
            let _ = held.recv_timeout(Duration::from_secs(3));
        });
        let until = Instant::now() + Duration::from_secs(1);
        let result = loop {
            if let Some(result) = job.ready() {
                break Some(result);
            }
            if Instant::now() >= until {
                break None;
            }
            std::thread::sleep(Duration::from_millis(1));
        };
        drop(job);
        let _ = release.send(());
        producer.join().unwrap();
        assert!(
            matches!(result, Some(Err(()))),
            "must reject before EOF or the two-second deadline"
        );
    }
}

use crate::cmdbuilder::CommandBuilder;
use crate::win::psuedocon::PsuedoCon;
use crate::{Child, MasterPty, PtyPair, PtySize, PtySystem, SlavePty};
use anyhow::Error;
use filedescriptor::{FileDescriptor, Pipe};
use std::io::{self, Write};
use std::os::windows::io::AsRawHandle;
use std::ptr;
use std::sync::{Arc, Mutex};
use winapi::um::namedpipeapi::SetNamedPipeHandleState;
use winapi::um::winbase::PIPE_NOWAIT;
use winapi::um::wincon::COORD;
use winapi::um::winnt::HANDLE;

#[derive(Default)]
pub struct ConPtySystem {}

impl PtySystem for ConPtySystem {
    fn openpty(&self, size: PtySize) -> anyhow::Result<PtyPair> {
        let stdin = Pipe::new()?;
        let stdout = Pipe::new()?;

        let con = PsuedoCon::new(
            COORD {
                X: size.cols as i16,
                Y: size.rows as i16,
            },
            stdin.read,
            stdout.write,
        )?;

        let master = ConPtyMasterPty {
            inner: Arc::new(Mutex::new(Inner {
                con,
                readable: stdout.read,
                writable: Some(stdin.write),
                size,
            })),
        };

        let slave = ConPtySlavePty {
            inner: master.inner.clone(),
        };

        Ok(PtyPair {
            master: Box::new(master),
            slave: Box::new(slave),
        })
    }
}

struct Inner {
    con: PsuedoCon,
    readable: FileDescriptor,
    writable: Option<FileDescriptor>,
    size: PtySize,
}

impl Inner {
    pub fn resize(
        &mut self,
        num_rows: u16,
        num_cols: u16,
        pixel_width: u16,
        pixel_height: u16,
    ) -> Result<(), Error> {
        self.con.resize(COORD {
            X: num_cols as i16,
            Y: num_rows as i16,
        })?;
        self.size = PtySize {
            rows: num_rows,
            cols: num_cols,
            pixel_width,
            pixel_height,
        };
        Ok(())
    }
}

#[derive(Clone)]
pub struct ConPtyMasterPty {
    inner: Arc<Mutex<Inner>>,
}

pub struct ConPtySlavePty {
    inner: Arc<Mutex<Inner>>,
}

impl ConPtyMasterPty {
    fn take_inner_writer(&self) -> anyhow::Result<FileDescriptor> {
        self.inner
            .lock()
            .unwrap()
            .writable
            .take()
            .ok_or_else(|| anyhow::anyhow!("writer already taken"))
    }
}

fn configure_pipe_nowait(writer: &FileDescriptor) -> anyhow::Result<()> {
    let mut mode = PIPE_NOWAIT;
    let configured = unsafe {
        SetNamedPipeHandleState(
            writer.as_raw_handle() as HANDLE,
            &mut mode,
            ptr::null_mut(),
            ptr::null_mut(),
        )
    };
    if configured == 0 {
        return Err(anyhow::anyhow!(
            "failed to enable nonblocking ConPTY input: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// A `PIPE_NOWAIT` byte-pipe writer with a conforming [`Write`] contract.
///
/// Windows reports a temporarily full nonblocking byte pipe as a successful
/// zero-byte write. Rust reserves `Ok(0)` on nonempty input for a writer that
/// can no longer accept bytes, so [`Write::write_all`] turns it into
/// `WriteZero`. Normalize that transient state to `WouldBlock`; callers can
/// then retry without mistaking backpressure for a permanent failure.
struct NonblockingPipeWriter {
    inner: FileDescriptor,
}

impl NonblockingPipeWriter {
    fn new(inner: FileDescriptor) -> anyhow::Result<Self> {
        configure_pipe_nowait(&inner)?;
        Ok(Self { inner })
    }
}

impl Write for NonblockingPipeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.inner.write(buf) {
            Ok(0) if !buf.is_empty() => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "ConPTY input pipe is temporarily full",
            )),
            result => result,
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl MasterPty for ConPtyMasterPty {
    fn resize(&self, size: PtySize) -> anyhow::Result<()> {
        let mut inner = self.inner.lock().unwrap();
        inner.resize(size.rows, size.cols, size.pixel_width, size.pixel_height)
    }

    fn get_size(&self) -> Result<PtySize, Error> {
        let inner = self.inner.lock().unwrap();
        Ok(inner.size)
    }

    fn try_clone_reader(&self) -> anyhow::Result<Box<dyn std::io::Read + Send>> {
        Ok(Box::new(self.inner.lock().unwrap().readable.try_clone()?))
    }

    fn take_writer(&self) -> anyhow::Result<Box<dyn std::io::Write + Send>> {
        Ok(Box::new(self.take_inner_writer()?))
    }

    fn take_nonblocking_writer(&self) -> anyhow::Result<Box<dyn std::io::Write + Send>> {
        let writer = self.take_inner_writer()?;
        Ok(Box::new(NonblockingPipeWriter::new(writer)?))
    }
}

impl SlavePty for ConPtySlavePty {
    fn spawn_command(&self, cmd: CommandBuilder) -> anyhow::Result<Box<dyn Child + Send + Sync>> {
        let inner = self.inner.lock().unwrap();
        let child = inner.con.spawn_command(cmd)?;
        Ok(Box::new(child))
    }
}

#[cfg(test)]
mod tests {
    use super::{NonblockingPipeWriter, Pipe};
    use std::io::Write;
    use std::time::{Duration, Instant};

    #[test]
    fn pipe_nowait_reports_would_block_when_full_and_recovers() {
        let pipe = Pipe::new().expect("create anonymous byte pipe");
        let mut reader = pipe.read;
        let mut writer =
            NonblockingPipeWriter::new(pipe.write).expect("enable conforming PIPE_NOWAIT writer");

        let fill_started = Instant::now();
        let initial = [b'x'; 1024];
        let initial_written = writer
            .write(&initial)
            .expect("write one ConPTY-sized nonblocking chunk");
        assert_eq!(
            initial_written,
            initial.len(),
            "a 1 KiB request must fit in an empty anonymous pipe"
        );
        let mut filled = initial_written;
        loop {
            let call_started = Instant::now();
            let result = writer.write(b"x");
            assert!(
                call_started.elapsed() < Duration::from_millis(250),
                "PIPE_NOWAIT write blocked for {:?}",
                call_started.elapsed()
            );
            match result {
                Ok(written) => {
                    assert_eq!(written, 1);
                    filled += written;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("unexpected nonblocking pipe error: {}", error),
            }
            assert!(
                filled < 1024 * 1024,
                "unread pipe accepted an implausibly large payload"
            );
        }
        assert!(filled > 0, "pipe accepted no initial input");
        assert!(
            fill_started.elapsed() < Duration::from_secs(2),
            "pipe saturation took {:?}",
            fill_started.elapsed()
        );

        let (byte_tx, byte_rx) = std::sync::mpsc::sync_channel(1);
        let (release_tx, release_rx) = std::sync::mpsc::sync_channel(0);
        let reader_thread = std::thread::spawn(move || {
            let mut byte = [0u8; 1];
            std::io::Read::read_exact(&mut reader, &mut byte).expect("read one pipe byte");
            byte_tx.send(byte).expect("publish read byte");
            release_rx.recv().expect("hold read handle");
        });
        let started = Instant::now();
        let written = loop {
            match writer.write(b"y") {
                Ok(written) => break written,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(error) => panic!("unexpected nonblocking pipe error: {}", error),
            }
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "writer never observed the pending pipe read"
            );
            std::thread::yield_now();
        };
        assert_eq!(written, 1);
        assert_eq!(byte_rx.recv().expect("receive read byte"), [b'x']);
        release_tx.send(()).expect("release read handle");
        reader_thread.join().expect("join pipe reader");
    }
}

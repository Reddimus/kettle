//! Incremental clipboard acquisition, also tested without a Wayland display.
use std::io::{self, Read};
use std::time::Instant;

pub(crate) struct Transfer {
    max: usize,
    deadline: Option<Instant>,
    content: Vec<u8>,
}

impl Transfer {
    pub(crate) fn new(max: usize, deadline: Instant) -> Self {
        Self { max, deadline: Some(deadline), content: Vec::new() }
    }

    pub(crate) fn unbounded() -> Self {
        Self { max: usize::MAX, deadline: None, content: Vec::new() }
    }

    pub(crate) fn read_ready(&mut self, reader: &mut impl Read) -> io::Result<Option<String>> {
        let mut scratch = [0; 4096];
        loop {
            if self.deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(io::ErrorKind::TimedOut.into());
            }
            let want =
                scratch.len().min(self.max.saturating_sub(self.content.len()).saturating_add(1));
            match reader.read(&mut scratch[..want]) {
                Ok(0) => {
                    // Lossy decoding can expand invalid bytes. Count before allocating.
                    let decoded_len = self
                        .content
                        .utf8_chunks()
                        .map(|chunk| {
                            chunk.valid().len() + usize::from(!chunk.invalid().is_empty()) * 3
                        })
                        .sum::<usize>();
                    if decoded_len > self.max {
                        return Err(io::ErrorKind::FileTooLarge.into());
                    }
                    return Ok(Some(String::from_utf8_lossy(&self.content).into_owned()));
                },
                Ok(n) => {
                    if n > self.max.saturating_sub(self.content.len()) {
                        return Err(io::ErrorKind::FileTooLarge.into());
                    }
                    self.content.try_reserve(n).map_err(|_| io::ErrorKind::OutOfMemory)?;
                    self.content.extend_from_slice(&scratch[..n]);
                },
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => (),
                Err(error) => return Err(error),
            }
        }
    }
}

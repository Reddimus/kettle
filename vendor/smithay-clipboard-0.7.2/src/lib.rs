//! Smithay Clipboard
//!
//! Provides access to the Wayland clipboard for gui applications. The user
//! should have surface around.

#![deny(clippy::all, clippy::if_not_else, clippy::enum_glob_use)]
use std::ffi::c_void;
use std::io::Result;
use std::sync::mpsc::{self, Receiver};

use sctk::reexports::calloop::channel::{self, Sender};
use sctk::reexports::client::backend::Backend;
use sctk::reexports::client::Connection;

mod bounded;
mod mime;
mod state;
mod worker;

/// Access to a Wayland clipboard.
pub struct Clipboard {
    request_sender: Sender<worker::Command>,
    request_receiver: Receiver<Result<String>>,
    ready: Receiver<Result<()>>,
    clipboard_thread: Option<std::thread::JoinHandle<()>>,
}

impl Clipboard {
    /// Creates new clipboard which will be running on its own thread with its
    /// own event queue to handle clipboard requests.
    ///
    /// # Safety
    ///
    /// `display` must be a valid `*mut wl_display` pointer, and it must remain
    /// valid for as long as `Clipboard` object is alive.
    pub unsafe fn new(display: *mut c_void) -> Self {
        let backend = unsafe { Backend::from_foreign_display(display.cast()) };
        let connection = Connection::from_backend(backend);

        // Create channel to send data to clipboard thread.
        let (request_sender, rx_chan) = channel::channel();
        // Create channel to get data from the clipboard thread.
        let (clipboard_reply_sender, request_receiver) = mpsc::channel();

        let (ready_tx, ready) = mpsc::channel();
        let name = String::from("smithay-clipboard");
        let clipboard_thread =
            worker::spawn(name, connection, rx_chan, clipboard_reply_sender, ready_tx);

        Self { request_receiver, request_sender, clipboard_thread, ready }
    }

    /// Wait for registry initialization, without loading any selection data.
    pub fn wait_ready(&self, timeout: std::time::Duration) -> Result<()> {
        self.ready.recv_timeout(timeout).map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => std::io::ErrorKind::TimedOut,
            mpsc::RecvTimeoutError::Disconnected => std::io::ErrorKind::NotConnected,
        })?
    }

    /// Load at most `max` bytes by an absolute deadline.
    pub fn load_bounded(&self, max: usize, deadline: std::time::Instant) -> Result<String> {
        self.request_sender
            .send(worker::Command::LoadBounded(false, max, deadline))
            .map_err(|_| std::io::ErrorKind::NotConnected)?;
        self.request_receiver.recv().map_err(|_| std::io::ErrorKind::NotConnected)?
    }

    /// Load PRIMARY at most `max` bytes by an absolute deadline.
    pub fn load_primary_bounded(&self, max: usize, deadline: std::time::Instant) -> Result<String> {
        self.request_sender
            .send(worker::Command::LoadBounded(true, max, deadline))
            .map_err(|_| std::io::ErrorKind::NotConnected)?;
        self.request_receiver.recv().map_err(|_| std::io::ErrorKind::NotConnected)?
    }

    /// Store a selection with acknowledgment, unless its deadline has passed.
    /// `primary` selects PRIMARY rather than CLIPBOARD.
    pub fn store_bounded(
        &self,
        primary: bool,
        text: String,
        deadline: std::time::Instant,
    ) -> Result<()> {
        self.request_sender
            .send(worker::Command::StoreBounded(primary, text, deadline))
            .map_err(|_| std::io::ErrorKind::NotConnected)?;
        self.request_receiver.recv().map_err(|_| std::io::ErrorKind::NotConnected)??;
        Ok(())
    }

    /// Load clipboard data.
    ///
    /// Loads content from a clipboard on a last observed seat.
    pub fn load(&self) -> Result<String> {
        let _ = self.request_sender.send(worker::Command::Load);

        if let Ok(reply) = self.request_receiver.recv() {
            reply
        } else {
            // The clipboard thread is dead, however we shouldn't crash downstream, so
            // propogating an error.
            Err(std::io::Error::other("clipboard is dead."))
        }
    }

    /// Store to a clipboard.
    ///
    /// Stores to a clipboard on a last observed seat.
    pub fn store<T: Into<String>>(&self, text: T) {
        let request = worker::Command::Store(text.into());
        let _ = self.request_sender.send(request);
    }

    /// Load primary clipboard data.
    ///
    /// Loads content from a  primary clipboard on a last observed seat.
    pub fn load_primary(&self) -> Result<String> {
        let _ = self.request_sender.send(worker::Command::LoadPrimary);

        if let Ok(reply) = self.request_receiver.recv() {
            reply
        } else {
            // The clipboard thread is dead, however we shouldn't crash downstream, so
            // propogating an error.
            Err(std::io::Error::other("clipboard is dead."))
        }
    }

    /// Store to a primary clipboard.
    ///
    /// Stores to a primary clipboard on a last observed seat.
    pub fn store_primary<T: Into<String>>(&self, text: T) {
        let request = worker::Command::StorePrimary(text.into());
        let _ = self.request_sender.send(request);
    }
}

impl Drop for Clipboard {
    fn drop(&mut self) {
        // Shutdown smithay-clipboard.
        let _ = self.request_sender.send(worker::Command::Exit);
        if let Some(clipboard_thread) = self.clipboard_thread.take() {
            let _ = clipboard_thread.join();
        }
    }
}

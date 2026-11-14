//! What a Wayland selection offers, and what a paste takes from it.
//!
//! A compositor lists every type of a selection before the client reads
//! anything, so a paste decides from that list and then reads each type it
//! needs once: no trial reads. The protocol objects stay behind [`Offered`]
//! and [`Owned`], so the rules here run against a loopback source in tests.
//!
//! | The offer lists | Policy | A paste gives |
//! |---|---|---|
//! | a file list of local files | `paste-files = on` | their paths |
//! | a file list of local files | `paste-files = off` | nothing, not even the text copy of the paths |
//! | a file list that is not all local files | either | the offer's text, if it has any |
//! | a malformed or oversize file list | either | a refusal |
//! | a text type | | the text |
//! | `image/png` and no text type | `paste-images = on` | the PNG bytes |
//!
//! `text/uri-list` is read in preference to `x-special/gnome-copied-files`.
//! Text wins over an image, as it does on every platform: a source that
//! offers both, such as spreadsheet cells, means the text.
use std::collections::BTreeMap;
use std::io::{Error, ErrorKind, Read, Result};
use std::os::fd::{AsFd, AsRawFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use super::uri_list::{self, UriList};
use crate::clipboard::{PasteContent, PasteImage, PasteRefusal, PasteWant};

pub(crate) const URI_LIST: &str = "text/uri-list";
pub(crate) const GNOME_COPIED_FILES: &str = "x-special/gnome-copied-files";
pub(crate) const PNG: &str = "image/png";
/// Text types, most preferred first.
pub(crate) const TEXT_MIMES: [&str; 5] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "TEXT",
    "STRING",
];
/// Sources kept for answering `send`; older ones are cancelled by the
/// compositor once a newer selection replaces them.
const MAX_SOURCES: usize = 4;

/// The types of one offer that Kettle reads. Each `offer` event sets a flag
/// and stores no string, so a source that lists thousands of types costs
/// nothing more.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct Kinds {
    /// The preferred text type the offer lists.
    pub(crate) text: Option<&'static str>,
    pub(crate) uri_list: bool,
    pub(crate) gnome_copied_files: bool,
    pub(crate) png: bool,
}

impl Kinds {
    pub(crate) fn note(&mut self, mime: &str) {
        match mime {
            URI_LIST => self.uri_list = true,
            GNOME_COPIED_FILES => self.gnome_copied_files = true,
            PNG => self.png = true,
            _ => self.text = better_text_mime(self.text, mime),
        }
    }

    /// Whether a paste could take anything from the offer.
    pub(crate) fn any(self) -> bool {
        self.text.is_some() || self.uri_list || self.gnome_copied_files || self.png
    }

    /// Only the text type: all a PRIMARY selection is read for.
    pub(crate) fn text_only(self) -> Self {
        Self {
            text: self.text,
            ..Self::default()
        }
    }
}

/// The preferred text type among `current` and `offered`.
pub(crate) fn better_text_mime(
    current: Option<&'static str>,
    offered: &str,
) -> Option<&'static str> {
    let rank = |mime: &str| TEXT_MIMES.iter().position(|known| *known == mime);
    let Some(new) = rank(offered) else {
        return current;
    };
    match current.and_then(rank) {
        Some(old) if old <= new => current,
        _ => Some(TEXT_MIMES[new]),
    }
}

/// A selection offer: `receive` asks its source to write one type to `fd`.
pub(crate) trait Offered: Clone {
    fn receive(&self, mime: &'static str, fd: BorrowedFd<'_>);
    fn discard(self);
}

/// A selection source this client created.
pub(crate) trait Owned {
    fn discard(self);
}

/// One selection channel, CLIPBOARD or PRIMARY: the compositor's current
/// offer and the sources behind the selections this client set.
pub(crate) struct Channel<O, S> {
    offer: Option<(O, Kinds)>,
    sources: BTreeMap<u64, (S, Arc<str>)>,
    owned: Option<u64>,
}

impl<O, S> Default for Channel<O, S> {
    fn default() -> Self {
        Self {
            offer: None,
            sources: BTreeMap::new(),
            owned: None,
        }
    }
}

/// What a read finds on a channel, taken under the lock and used without it.
pub(crate) enum Held<O> {
    /// This client owns the selection. Its `send` event is dispatched on the
    /// UI thread, which is waiting for the read, so the text is answered from
    /// memory.
    Own(Arc<str>),
    Offer(O, Kinds),
    Empty,
}

impl<O: Offered, S: Owned> Channel<O, S> {
    /// The compositor's new offer (or none), replacing the last one.
    pub(crate) fn set_offer(&mut self, offer: Option<(O, Kinds)>) {
        if let Some((old, _)) = std::mem::replace(&mut self.offer, offer) {
            old.discard();
        }
    }

    /// Own the selection with `text` through `source`, already set on the
    /// device.
    pub(crate) fn own(&mut self, id: u64, source: S, text: Arc<str>) {
        self.sources.insert(id, (source, text));
        self.owned = Some(id);
        while self.sources.len() > MAX_SOURCES {
            if let Some((_, (old, _))) = self.sources.pop_first() {
                old.discard();
            }
        }
    }

    pub(crate) fn text_for(&self, id: u64) -> Option<Arc<str>> {
        self.sources.get(&id).map(|(_, text)| text.clone())
    }

    pub(crate) fn cancelled(&mut self, id: u64) {
        if self.owned == Some(id) {
            self.owned = None;
        }
        if let Some((source, _)) = self.sources.remove(&id) {
            source.discard();
        }
    }

    pub(crate) fn close(&mut self) {
        self.set_offer(None);
        for (_, (source, _)) in std::mem::take(&mut self.sources) {
            source.discard();
        }
        self.owned = None;
    }

    pub(crate) fn held(&self) -> Held<O> {
        if let Some((_, text)) = self.owned.and_then(|id| self.sources.get(&id)) {
            return Held::Own(text.clone());
        }
        match &self.offer {
            Some((offer, kinds)) => Held::Offer(offer.clone(), *kinds),
            None => Held::Empty,
        }
    }
}

/// How a read reaches the compositor and when it must have ended.
pub(crate) struct Transfer<'a> {
    pub(crate) flush: &'a dyn Fn() -> Result<()>,
    pub(crate) deadline: Instant,
}

impl Transfer<'_> {
    /// Ask `offer` for `mime` and read it to end of file: at most `max` bytes
    /// (one more is an error) by the deadline. `mime` is always one of this
    /// module's constants, never a string the source supplied.
    pub(crate) fn receive<O: Offered>(
        &self,
        offer: &O,
        mime: &'static str,
        max: usize,
    ) -> Result<Vec<u8>> {
        let (mut reader, writer) = UnixStream::pair()?;
        offer.receive(mime, writer.as_fd());
        (self.flush)()?;
        drop(writer);
        read_bounded(&mut reader, max, self.deadline)
    }
}

fn too_large() -> Error {
    Error::new(ErrorKind::FileTooLarge, "selection exceeds the cap")
}

/// The channel's text: at most `max` bytes (one more is an error). Invalid
/// UTF-8 is replaced rather than refused.
pub(crate) fn load_text<O: Offered>(
    held: Held<O>,
    max: usize,
    transfer: &Transfer<'_>,
) -> Result<String> {
    match held {
        Held::Own(text) if text.len() > max => Err(too_large()),
        Held::Own(text) => Ok(text.to_string()),
        Held::Offer(offer, kinds) => {
            let mime = kinds
                .text
                .ok_or_else(|| Error::new(ErrorKind::NotFound, "the selection has no text"))?;
            let bytes = transfer.receive(&offer, mime, max)?;
            Ok(String::from_utf8_lossy(&bytes).into_owned())
        }
        Held::Empty => Err(Error::new(ErrorKind::NotFound, "the selection is empty")),
    }
}

/// A selection read's failure as the clipboard layer names it.
pub(crate) fn selection_error(error: &Error) -> arboard::Error {
    match error.kind() {
        ErrorKind::TimedOut => arboard::Error::Timeout,
        ErrorKind::FileTooLarge | ErrorKind::OutOfMemory => arboard::Error::LimitExceeded,
        _ => arboard::Error::ContentNotAvailable,
    }
}

/// The file list of an offer that has one, classified.
fn file_list<O: Offered>(offer: &O, kinds: Kinds, transfer: &Transfer<'_>) -> Result<UriList> {
    // A list over the cap fails the read; it is never cut short into a
    // different, valid list.
    let max = uri_list::MAX_BYTES;
    if kinds.uri_list {
        transfer
            .receive(offer, URI_LIST, max)
            .map(|bytes| uri_list::classify(&bytes))
    } else {
        transfer
            .receive(offer, GNOME_COPIED_FILES, max)
            .map(|bytes| uri_list::classify_gnome_copied_files(&bytes))
    }
}

/// The local files the channel's offer names, for a preview of what a paste
/// would bring. `None` for anything else.
pub(crate) fn local_files<O: Offered>(
    held: Held<O>,
    transfer: &Transfer<'_>,
) -> Option<Vec<PathBuf>> {
    let Held::Offer(offer, kinds) = held else {
        return None;
    };
    if !(kinds.uri_list || kinds.gnome_copied_files) {
        return None;
    }
    match file_list(&offer, kinds, transfer) {
        Ok(UriList::Local(paths)) => Some(paths),
        _ => None,
    }
}

/// What a paste takes from the channel: the table at the top of this module.
pub(crate) fn paste<O: Offered>(
    held: Held<O>,
    want: PasteWant,
    text_max: usize,
    transfer: &Transfer<'_>,
) -> PasteContent {
    let (offer, kinds) = match held {
        // A selection this client owns offers text only.
        own @ Held::Own(_) => {
            return PasteContent::Text(
                load_text::<O>(own, text_max, transfer).map_err(|error| selection_error(&error)),
            );
        }
        Held::Offer(offer, kinds) => (offer, kinds),
        Held::Empty => return PasteContent::Text(Err(arboard::Error::ContentNotAvailable)),
    };
    if kinds.uri_list || kinds.gnome_copied_files {
        match (file_list(&offer, kinds, transfer), want.files) {
            (Ok(UriList::Local(paths)), true) => return PasteContent::Files(paths),
            // The copied files are not pasted, and neither is the plain-text
            // copy of their paths that a file manager adds beside the list.
            (Ok(UriList::Local(_) | UriList::Foreign { local: true }), false) => {
                return PasteContent::FilesOff;
            }
            // Links, remote files or nothing: not a file list to Kettle.
            (Ok(UriList::Foreign { .. } | UriList::Empty), _) => {}
            (Err(error), _) if error.kind() == ErrorKind::TimedOut => {
                return PasteContent::Text(Err(arboard::Error::Timeout));
            }
            (Ok(UriList::Malformed) | Err(_), _) => {
                return PasteContent::Refused(PasteRefusal::Files);
            }
        }
    }
    if kinds.text.is_some() {
        return PasteContent::Text(
            load_text(Held::Offer(offer, kinds), text_max, transfer)
                .map_err(|error| selection_error(&error)),
        );
    }
    if kinds.png && want.images {
        // One byte past the cap is read so that an oversize image is refused
        // instead of being cut short.
        let max = crate::paste_image::PNG_SOURCE_MAX;
        return match transfer.receive(&offer, PNG, max + 1) {
            Ok(bytes) if !bytes.is_empty() && bytes.len() <= max => {
                PasteContent::Image(PasteImage::Png(bytes))
            }
            Err(error) if error.kind() == ErrorKind::TimedOut => {
                PasteContent::Text(Err(arboard::Error::Timeout))
            }
            Ok(_) | Err(_) => PasteContent::Refused(PasteRefusal::Image),
        };
    }
    PasteContent::Text(Err(arboard::Error::ContentNotAvailable))
}

/// Read to end of file within `max` bytes and `deadline`. A source that
/// stops writing costs the caller the deadline and no more.
pub(crate) fn read_bounded(
    reader: &mut UnixStream,
    max: usize,
    deadline: Instant,
) -> Result<Vec<u8>> {
    let timed_out = || Error::new(ErrorKind::TimedOut, "selection read timed out");
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 16 * 1024];
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(timed_out());
        }
        // Wait with poll, not a receive timeout: setting one fails on some
        // systems once the writer has closed, with its data still to read.
        let mut poll = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let wait = i32::try_from(left.as_millis().max(1)).unwrap_or(i32::MAX);
        // SAFETY: one initialized pollfd for a descriptor `reader` keeps open.
        match unsafe { libc::poll(&mut poll, 1, wait) } {
            0 => continue,
            n if n < 0 => {
                let error = Error::last_os_error();
                if error.kind() == ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            _ => {}
        }
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if n > max.saturating_sub(bytes.len()) {
                    return Err(too_large());
                }
                bytes.extend_from_slice(&chunk[..n]);
            }
            Err(error) if error.kind() == ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests;

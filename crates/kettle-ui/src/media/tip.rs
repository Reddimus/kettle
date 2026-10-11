//! The one-time tip that an inline card opens on a click. The first card a
//! user ever sees says so for a few seconds; a small per-user file beside
//! the update check's records that it has, so later sessions stay quiet.
//! The record is created exclusively before the tip shows, so of two Kettles
//! running at once only the first to create it shows the tip.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kettle_core::InlineNonce;

/// How long the tip shows on its card.
pub(crate) const TIP_TIME: Duration = Duration::from_secs(10);
/// The most the tip file is read; it holds one small JSON object.
const MAX_TIP_FILE_BYTES: u64 = 256;
const TIP_FILE: &str = "ui-tips.json";

#[derive(serde::Deserialize, serde::Serialize)]
struct TipFile {
    version: u8,
    inline_cards: bool,
}

/// Where the tip stands in this process.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CardsTip {
    /// Not shown to this user yet: the next card on screen shows it.
    Waiting,
    /// On `nonce`'s card in `pane`, until `until`.
    Showing {
        pane: u64,
        nonce: InlineNonce,
        until: Instant,
    },
    /// Shown before, or done.
    Done,
}

impl CardsTip {
    /// The tip as this user left it: done once they have seen it.
    pub(crate) fn load() -> Self {
        if tip_path().is_some_and(|path| seen(&path)) {
            Self::Done
        } else {
            Self::Waiting
        }
    }

    /// Show the tip on the first of `cards` (pane and nonce, in paint
    /// order), if it is waiting and no other Kettle has recorded it, and
    /// record it. Returns the card it began on.
    pub(crate) fn begin(
        &mut self,
        cards: impl Iterator<Item = (u64, InlineNonce)>,
        now: Instant,
    ) -> Option<(u64, InlineNonce)> {
        self.begin_at(cards, now, tip_path)
    }

    /// [`Self::begin`], recording at `path` once there is a card to show it
    /// on.
    fn begin_at(
        &mut self,
        mut cards: impl Iterator<Item = (u64, InlineNonce)>,
        now: Instant,
        path: impl FnOnce() -> Option<PathBuf>,
    ) -> Option<(u64, InlineNonce)> {
        if *self != Self::Waiting {
            return None;
        }
        let (pane, nonce) = cards.next()?;
        // Another Kettle may have shown it since this one started. A few
        // small file operations, once per user.
        if path().is_some_and(|path| !claim(&path)) {
            *self = Self::Done;
            return None;
        }
        *self = Self::Showing {
            pane,
            nonce,
            until: now + TIP_TIME,
        };
        Some((pane, nonce))
    }

    /// When the tip ends, while it shows.
    pub(crate) fn until(&self) -> Option<Instant> {
        match *self {
            Self::Showing { until, .. } => Some(until),
            _ => None,
        }
    }

    /// End the tip: its time is up, or a card was opened. Returns the pane
    /// it showed in, to repaint.
    pub(crate) fn end(&mut self) -> Option<u64> {
        let pane = match *self {
            Self::Showing { pane, .. } => Some(pane),
            Self::Waiting => None,
            Self::Done => return None,
        };
        *self = Self::Done;
        pane
    }
}

fn tip_path() -> Option<PathBuf> {
    kettle_config::Config::default_path()
        .and_then(|path| path.parent().map(|directory| directory.join(TIP_FILE)))
}

/// Whether `path` records the tip as seen. A missing, oversized or unreadable
/// file means not yet, as does anything but a regular file Kettle could have
/// written there, which is opened without following a link or waiting on it.
fn seen(path: &Path) -> bool {
    use std::io::Read as _;
    let Ok(file) = kettle_state::open_trusted_file_read(path) else {
        return false;
    };
    let mut bytes = Vec::new();
    if file
        .take(MAX_TIP_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .is_err()
        || bytes.len() as u64 > MAX_TIP_FILE_BYTES
    {
        return false;
    }
    serde_json::from_slice::<TipFile>(&bytes).is_ok_and(|tip| tip.version == 1 && tip.inline_cards)
}

/// Record at `path` that the tip shows, unless something is already there:
/// false then. The record is written whole and published by an exclusive
/// link, so only one process can create it and none ever sees it half
/// written. An entry already there, a record or not, means the tip has shown
/// or Kettle cannot tell, so it does not show again; a record that cannot be
/// written leaves the tip to show unrecorded.
fn claim(path: &Path) -> bool {
    let Ok(json) = serde_json::to_vec(&TipFile {
        version: 1,
        inline_cards: true,
    }) else {
        return true;
    };
    match kettle_state::atomic_create_new(path, &json, kettle_state::AtomicWriteOptions::PRIVATE) {
        Ok(created) => created,
        // A link or something else that is not a regular file is there.
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => false,
        Err(error) => {
            log::warn!("tip: could not record that it was shown: {error}");
            true
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tip shows once, on the first card on screen, for its time; it is
    /// remembered across sessions, only the Kettle that records it shows
    /// it, and an entry it cannot read stops it without being touched.
    #[test]
    fn the_tip_shows_once_on_the_first_card_and_is_remembered() {
        let nonce = InlineNonce::new([1, 2, 3, 4, 5, 6]).unwrap();
        let other = InlineNonce::new([6, 5, 4, 3, 2, 1]).unwrap();
        let now = Instant::now();
        let mut tip = CardsTip::Waiting;
        // No path: a test never records into the user's own directory.
        let none = || None;
        assert_eq!(
            tip.begin_at(std::iter::empty(), now, none),
            None,
            "no card, no tip"
        );
        assert_eq!(
            tip.begin_at([(7, nonce), (8, other)].into_iter(), now, none),
            Some((7, nonce))
        );
        assert_eq!(tip.until(), Some(now + TIP_TIME));
        assert_eq!(
            tip.begin_at([(8, other)].into_iter(), now, none),
            None,
            "once"
        );
        assert_eq!(tip.end(), Some(7));
        assert_eq!((tip, tip.end()), (CardsTip::Done, None));

        let dir = kettle_test_support::private_tempdir("kettle-tip-");
        let path = dir.path().join(TIP_FILE);
        assert!(!seen(&path));
        // Of two Kettles that both loaded before either showed it, only the
        // first to record it shows it.
        let (mut first, mut second) = (CardsTip::Waiting, CardsTip::Waiting);
        assert_eq!(
            first.begin_at([(7, nonce)].into_iter(), now, || Some(path.clone())),
            Some((7, nonce))
        );
        assert!(seen(&path), "recorded before it shows");
        assert_eq!(
            second.begin_at([(7, nonce)].into_iter(), now, || Some(path.clone())),
            None
        );
        assert_eq!(second, CardsTip::Done);
        // A record Kettle cannot read is not seen, but it is left alone and
        // no tip shows: Kettle cannot tell it from another's.
        for unreadable in [
            b"{not json".to_vec(),
            vec![b' '; MAX_TIP_FILE_BYTES as usize + 1],
            Vec::new(),
        ] {
            std::fs::write(&path, &unreadable).unwrap();
            assert!(!seen(&path));
            assert!(!claim(&path));
            assert_eq!(std::fs::read(&path).unwrap(), unreadable, "left alone");
            let mut tip = CardsTip::Waiting;
            assert_eq!(
                tip.begin_at([(7, nonce)].into_iter(), now, || Some(path.clone())),
                None
            );
        }
    }

    /// Only a regular file counts: a link to a record, even a valid one, is
    /// not read, and neither is a pipe, which would block the window.
    #[cfg(unix)]
    #[test]
    fn only_a_regular_tip_file_is_read() {
        let dir = kettle_test_support::private_tempdir("kettle-tip-");
        let record = dir.path().join("record.json");
        assert!(claim(&record));
        assert!(seen(&record));
        let link = dir.path().join(TIP_FILE);
        std::os::unix::fs::symlink(&record, &link).unwrap();
        assert!(!seen(&link));
        assert!(!claim(&link), "and no tip over a link");
        let pipe = dir.path().join("pipe.json");
        let name = std::ffi::CString::new(pipe.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `name` is a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(!seen(&pipe));
    }
}

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
    Pending {
        pane: u64,
        nonce: InlineNonce,
    },
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

    /// Reserve the first shown card while a worker claims the durable record.
    pub(crate) fn begin(
        &mut self,
        cards: impl Iterator<Item = (u64, InlineNonce)>,
        complete: impl FnOnce(bool) + Send + 'static,
    ) {
        self.begin_with(cards, tip_path, claim, complete);
    }

    fn begin_with(
        &mut self,
        mut cards: impl Iterator<Item = (u64, InlineNonce)>,
        path: impl FnOnce() -> Option<PathBuf> + Send + 'static,
        mut write: impl FnMut(&Path) -> std::io::Result<bool> + Send + 'static,
        complete: impl FnOnce(bool) + Send + 'static,
    ) -> bool {
        if *self != Self::Waiting {
            return false;
        }
        let Some((pane, nonce)) = cards.next() else {
            return false;
        };
        *self = Self::Pending { pane, nonce };
        if let Err(error) = std::thread::Builder::new()
            .name("kettle-tip-claim".into())
            .spawn(move || {
                let claimed = match path() {
                    None => true,
                    Some(path) => {
                        let mut claimed = false;
                        for _ in 0..2 {
                            match write(&path) {
                                Ok(created) => {
                                    claimed = created;
                                    break;
                                }
                                Err(error) => {
                                    log::warn!("tip: could not record that it was shown: {error}")
                                }
                            }
                        }
                        claimed
                    }
                };
                complete(claimed);
            })
        {
            log::warn!("tip: claim worker could not start: {error}");
            *self = Self::Done;
            return false;
        }
        true
    }

    pub(crate) fn pending_card(&self) -> Option<(u64, InlineNonce)> {
        match *self {
            Self::Pending { pane, nonce } => Some((pane, nonce)),
            _ => None,
        }
    }

    pub(crate) fn complete(
        &mut self,
        claimed: bool,
        visible: bool,
        now: Instant,
    ) -> Option<(u64, InlineNonce)> {
        let (pane, nonce) = self.pending_card()?;
        *self = Self::Done;
        if !claimed || !visible {
            return None;
        }
        *self = Self::Showing {
            pane,
            nonce,
            until: now + TIP_TIME,
        };
        Some((pane, nonce))
    }

    #[cfg(test)]
    fn begin_at(
        &mut self,
        cards: impl Iterator<Item = (u64, InlineNonce)>,
        now: Instant,
        path: impl FnOnce() -> Option<PathBuf> + Send + 'static,
    ) -> Option<(u64, InlineNonce)> {
        let (send, receive) = std::sync::mpsc::channel();
        if !self.begin_with(cards, path, claim, move |claimed| {
            send.send(claimed).unwrap()
        }) {
            return None;
        }
        self.complete(
            receive.recv_timeout(Duration::from_secs(5)).unwrap(),
            true,
            now,
        )
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
            Self::Waiting | Self::Pending { .. } => None,
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

/// Publish one private record exclusively; an existing entry suppresses the tip.
fn claim(path: &Path) -> std::io::Result<bool> {
    let json = serde_json::to_vec(&TipFile {
        version: 1,
        inline_cards: true,
    })?;
    match kettle_state::atomic_create_new(path, &json, kettle_state::AtomicWriteOptions::PRIVATE) {
        Err(error) if error.kind() == std::io::ErrorKind::InvalidInput => Ok(false),
        result => result,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_path(path: &Path) -> impl FnOnce() -> Option<PathBuf> + Send + 'static {
        let path = path.to_path_buf();
        move || Some(path)
    }

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
            first.begin_at([(7, nonce)].into_iter(), now, test_path(&path)),
            Some((7, nonce))
        );
        assert!(seen(&path), "recorded before it shows");
        assert_eq!(
            second.begin_at([(7, nonce)].into_iter(), now, test_path(&path)),
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
            assert!(!claim(&path).unwrap());
            assert_eq!(std::fs::read(&path).unwrap(), unreadable, "left alone");
            let mut tip = CardsTip::Waiting;
            assert_eq!(
                tip.begin_at([(7, nonce)].into_iter(), now, test_path(&path)),
                None
            );
        }
    }

    #[test]
    fn durable_tip_claim_does_not_run_on_caller_thread() {
        let caller = std::thread::current().id();
        let nonce = InlineNonce::new([1, 2, 3, 4, 5, 6]).unwrap();
        let mut tip = CardsTip::Waiting;
        tip.begin_at([(7, nonce)].into_iter(), Instant::now(), move || {
            assert_ne!(
                caller,
                std::thread::current().id(),
                "durable tip path and claim must run off the event loop"
            );
            None
        });
    }

    #[test]
    fn pending_tip_is_reserved_and_its_deadline_starts_on_completion() {
        let nonce = InlineNonce::new([1, 2, 3, 4, 5, 6]).unwrap();
        let now = Instant::now();
        let mut tip = CardsTip::Waiting;
        let (release, blocked) = std::sync::mpsc::channel();
        let (send, receive) = std::sync::mpsc::channel();
        assert!(tip.begin_with(
            [(7, nonce)].into_iter(),
            || Some(PathBuf::from("unused")),
            move |_| {
                blocked.recv().unwrap();
                Ok(true)
            },
            move |claimed| send.send(claimed).unwrap()
        ));
        assert_eq!(tip.pending_card(), Some((7, nonce)));
        assert_eq!(tip.until(), None);
        assert!(!tip.begin_with(
            [(8, nonce)].into_iter(),
            || panic!("duplicate path"),
            |_| panic!("duplicate claim"),
            |_| panic!("duplicate completion")
        ));
        release.send(()).unwrap();
        let claimed = receive.recv_timeout(Duration::from_secs(5)).unwrap();
        let completed = now + Duration::from_secs(30);
        assert_eq!(tip.complete(claimed, true, completed), Some((7, nonce)));
        assert_eq!(tip.until(), Some(completed + TIP_TIME));
        assert_eq!(tip.complete(true, true, completed), None);

        let mut hidden = CardsTip::Pending { pane: 7, nonce };
        assert_eq!(hidden.complete(true, false, completed), None);
        assert_eq!(hidden, CardsTip::Done, "a hidden card cannot start the tip");
        let mut opened = CardsTip::Pending { pane: 7, nonce };
        opened.end();
        assert_eq!(
            opened.complete(true, true, completed),
            None,
            "completion cannot resurrect an opened tip"
        );
    }

    #[test]
    fn failed_tip_claim_retries_once_and_stays_claimed_in_memory() {
        let nonce = InlineNonce::new([1, 2, 3, 4, 5, 6]).unwrap();
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let worker_calls = calls.clone();
        let (send, receive) = std::sync::mpsc::channel();
        let mut tip = CardsTip::Waiting;
        assert!(tip.begin_with(
            [(7, nonce)].into_iter(),
            || Some(PathBuf::from("unused")),
            move |_| {
                worker_calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                Err(std::io::Error::other("stalled storage"))
            },
            move |claimed| send.send(claimed).unwrap()
        ));
        assert!(!receive.recv_timeout(Duration::from_secs(5)).unwrap());
        assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 2);
        assert_eq!(tip.complete(false, true, Instant::now()), None);
        assert_eq!(tip, CardsTip::Done);
        assert!(!tip.begin_with(
            [(7, nonce)].into_iter(),
            || panic!("no third attempt"),
            |_| panic!("no third write"),
            |_| panic!("no third completion")
        ));
    }

    #[test]
    fn tip_claim_can_succeed_on_its_single_retry() {
        let nonce = InlineNonce::new([1, 2, 3, 4, 5, 6]).unwrap();
        let (send, receive) = std::sync::mpsc::channel();
        let mut writes = 0;
        let mut tip = CardsTip::Waiting;
        assert!(tip.begin_with(
            [(7, nonce)].into_iter(),
            || Some(PathBuf::from("unused")),
            move |_| {
                writes += 1;
                if writes == 1 {
                    Err(std::io::Error::other("temporary failure"))
                } else {
                    Ok(true)
                }
            },
            move |claimed| send.send(claimed).unwrap()
        ));
        assert!(receive.recv_timeout(Duration::from_secs(5)).unwrap());
        assert_eq!(tip.complete(true, true, Instant::now()), Some((7, nonce)));
    }

    /// Only a regular file counts: a link to a record, even a valid one, is
    /// not read, and neither is a pipe, which would block the window.
    #[cfg(unix)]
    #[test]
    fn only_a_regular_tip_file_is_read() {
        let dir = kettle_test_support::private_tempdir("kettle-tip-");
        let record = dir.path().join("record.json");
        assert!(claim(&record).unwrap());
        assert!(seen(&record));
        let link = dir.path().join(TIP_FILE);
        std::os::unix::fs::symlink(&record, &link).unwrap();
        assert!(!seen(&link));
        assert!(!claim(&link).unwrap(), "and no tip over a link");
        let pipe = dir.path().join("pipe.json");
        let name = std::ffi::CString::new(pipe.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `name` is a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(!seen(&pipe));
    }
}

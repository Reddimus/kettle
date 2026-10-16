//! One presence writer, with one pending color per admitted window.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};
use std::thread::JoinHandle;

use kettle_ctl::presence::{PresenceEntry, PresenceGuard, claim};

const MAX_CLAIMS: usize = 1024;
type Slots = Arc<Mutex<Vec<Option<Slot>>>>;

struct Slot {
    dir: PathBuf,
    entry: PresenceEntry,
    guard: Option<PresenceGuard>,
    dirty: bool,
    closed: bool,
}

struct Publisher {
    slots: Slots,
    wake: SyncSender<()>,
    capacity: usize,
}

pub(crate) struct AccentPresence {
    publisher: Arc<Publisher>,
    index: usize,
}

impl AccentPresence {
    pub(crate) fn claim(dir: &Path, entry: PresenceEntry) -> Option<Self> {
        static PUBLISHER: OnceLock<Option<Arc<Publisher>>> = OnceLock::new();
        PUBLISHER
            .get_or_init(|| {
                Publisher::spawn(MAX_CLAIMS, publish_claim)
                    .ok()
                    .map(|(publisher, _worker)| publisher)
            })
            .as_ref()?
            .register(dir, entry)
    }

    pub(crate) fn set_rgb(&mut self, rgb: &str) {
        let mut slots = self
            .publisher
            .slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let slot = slots[self.index].as_mut().unwrap();
        slot.entry.rgb = rgb.to_owned();
        slot.dirty = true;
        drop(slots);
        let _ = self.publisher.wake.try_send(());
    }

    #[cfg(test)]
    pub(super) fn with_backend(
        dir: &Path,
        entry: PresenceEntry,
        publish: impl FnMut(&mut Option<PresenceGuard>, &Path, &PresenceEntry) + Send + 'static,
    ) -> (Self, JoinHandle<()>) {
        let (publisher, worker) = Publisher::spawn(MAX_CLAIMS, publish).unwrap();
        (publisher.register(dir, entry).unwrap(), worker)
    }
}

impl Drop for AccentPresence {
    fn drop(&mut self) {
        let mut slots = self
            .publisher
            .slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let slot = slots[self.index].as_mut().unwrap();
        slot.closed = true;
        slot.dirty = true;
        drop(slots);
        let _ = self.publisher.wake.try_send(());
    }
}

pub(super) fn publish_claim(guard: &mut Option<PresenceGuard>, dir: &Path, entry: &PresenceEntry) {
    if let Some(guard) = guard {
        guard.set_rgb(&entry.rgb);
    } else {
        *guard = claim(dir, entry.clone());
    }
}

impl Publisher {
    fn register(self: &Arc<Self>, dir: &Path, entry: PresenceEntry) -> Option<AccentPresence> {
        let mut slots = self.slots.lock().unwrap_or_else(PoisonError::into_inner);
        let existing = slots.iter().position(|slot| {
            slot.as_ref().is_some_and(|slot| {
                slot.dir == dir && slot.entry.pid == entry.pid && slot.entry.win == entry.win
            })
        });
        let index = if let Some(index) = existing {
            let slot = slots[index].as_mut().unwrap();
            if !slot.closed {
                return None;
            }
            slot.entry = entry;
            slot.closed = false;
            slot.dirty = true;
            index
        } else {
            let index = slots
                .iter()
                .position(Option::is_none)
                .unwrap_or(slots.len());
            if index == slots.len() {
                if slots.len() == self.capacity {
                    return None;
                }
                slots.push(None);
            }
            slots[index] = Some(Slot {
                dir: dir.to_owned(),
                entry,
                guard: None,
                dirty: true,
                closed: false,
            });
            index
        };
        drop(slots);
        let _ = self.wake.try_send(());
        Some(AccentPresence {
            publisher: Arc::clone(self),
            index,
        })
    }

    fn spawn(
        capacity: usize,
        mut publish: impl FnMut(&mut Option<PresenceGuard>, &Path, &PresenceEntry) + Send + 'static,
    ) -> std::io::Result<(Arc<Self>, JoinHandle<()>)> {
        let slots: Slots = Arc::new(Mutex::new(Vec::new()));
        let worker_slots = Arc::clone(&slots);
        let (wake, requested) = sync_channel(1);
        let worker = std::thread::Builder::new()
            .name("kettle-presence".into())
            .spawn(move || {
                let mut cursor = 0;
                while requested.recv().is_ok() {
                    loop {
                        let (index, mut guard, dir, entry) = {
                            let mut slots =
                                worker_slots.lock().unwrap_or_else(PoisonError::into_inner);
                            let len = slots.len();
                            let Some(index) =
                                (0..len)
                                    .map(|offset| (cursor + offset) % len)
                                    .find(|&index| {
                                        slots[index].as_ref().is_some_and(|slot| slot.dirty)
                                    })
                            else {
                                break;
                            };
                            cursor = (index + 1) % len;
                            let slot = slots[index].as_mut().unwrap();
                            slot.dirty = false;
                            (
                                index,
                                slot.guard.take(),
                                slot.dir.clone(),
                                (!slot.closed).then(|| slot.entry.clone()),
                            )
                        };
                        // Keep the slot reserved across I/O. Reopening the same
                        // filename reuses it, so an old removal cannot race a
                        // new claim, even when accents are disabled then enabled.
                        let removing = entry.is_none();
                        if let Some(entry) = entry {
                            publish(&mut guard, &dir, &entry);
                        } else {
                            drop(guard.take());
                        }
                        let mut slots = worker_slots.lock().unwrap_or_else(PoisonError::into_inner);
                        let slot = slots[index].as_mut().unwrap();
                        slot.guard = guard;
                        if removing && slot.closed {
                            slots[index] = None;
                        }
                    }
                }
            })?;
        Ok((
            Arc::new(Self {
                slots,
                wake,
                capacity,
            }),
            worker,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kettle_ctl::presence::live_entries;
    use std::sync::mpsc;
    use std::time::Duration;

    fn entry(win: u64, rgb: &str) -> PresenceEntry {
        PresenceEntry::claiming(std::process::id(), win, rgb.into(), true)
    }

    #[test]
    fn closing_during_a_write_discards_pending_updates_and_removes_the_claim() {
        let dir = crate::test_tempdir();
        let (entered, started) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (written, writes) = mpsc::channel();
        let (mut presence, worker) =
            AccentPresence::with_backend(&dir, entry(1, "#112233"), move |guard, dir, entry| {
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(5)).unwrap();
                publish_claim(guard, dir, entry);
                written.send(entry.rgb.clone()).unwrap();
            });
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        presence.set_rgb("#445566");
        drop(presence);
        release.send(()).unwrap();
        worker.join().unwrap();
        assert_eq!(writes.into_iter().collect::<Vec<_>>(), ["#112233"]);
        assert!(live_entries(&dir).is_empty());
    }

    #[test]
    fn reopening_during_a_write_preserves_the_replacement_claim() {
        let dir = crate::test_tempdir();
        let (entered, started) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let (written, writes) = mpsc::channel();
        let mut first = true;
        let (publisher, worker) = Publisher::spawn(1, move |guard, dir, entry| {
            if first {
                first = false;
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(5)).unwrap();
            }
            publish_claim(guard, dir, entry);
            written.send(entry.rgb.clone()).unwrap();
        })
        .unwrap();
        let old = publisher.register(&dir, entry(1, "#112233")).unwrap();
        started.recv_timeout(Duration::from_secs(5)).unwrap();
        drop(old);
        let replacement = publisher.register(&dir, entry(1, "#445566")).unwrap();
        release.send(()).unwrap();
        assert_eq!(
            writes.recv_timeout(Duration::from_secs(5)).unwrap(),
            "#112233"
        );
        assert_eq!(
            writes.recv_timeout(Duration::from_secs(5)).unwrap(),
            "#445566"
        );
        assert_eq!(live_entries(&dir)[0].rgb, "#445566");
        drop(replacement);
        drop(publisher);
        worker.join().unwrap();
        assert!(live_entries(&dir).is_empty());
    }

    #[test]
    fn admission_is_bounded() {
        let dir = crate::test_tempdir();
        let (publisher, worker) = Publisher::spawn(1, publish_claim).unwrap();
        let first = publisher.register(&dir, entry(1, "#010203")).unwrap();
        assert!(publisher.register(&dir, entry(2, "#010203")).is_none());
        drop(first);
        drop(publisher);
        worker.join().unwrap();
        assert!(live_entries(&dir).is_empty());
    }
}

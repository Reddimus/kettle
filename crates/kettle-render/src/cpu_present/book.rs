//! Buffer bookkeeping, apart from the protocol: which buffers exist, which
//! the compositor holds, how old each one's contents are, and which can go.
//!
//! `T` is the backing (a wl_shm buffer in production, `()` in tests).

use std::time::{Duration, Instant};

use super::PixelFormat;

/// Buffers kept at most. Two are enough when the compositor releases the
/// front buffer before the next frame; the third covers a compositor that
/// holds it a frame longer, so drawing never waits for a release.
pub const MAX_BUFFERS: usize = 3;

/// A spare buffer unused for this long is released by
/// [`SlotBook::idle_spares`], so an idle window holds one buffer.
pub const IDLE_RELEASE: Duration = Duration::from_secs(1);

/// How long after its last use a spare the compositor still holds keeps
/// the idle timer armed. Its release wakes nobody by itself, so the
/// presenter checks again every [`IDLE_RELEASE`] until then; a buffer held
/// longer goes at the next frame.
pub const BUSY_RECHECK: Duration = Duration::from_secs(10);

#[derive(Debug)]
pub struct Slot<T> {
    pub id: u64,
    pub width: u32,
    pub height: u32,
    pub format: PixelFormat,
    /// Attached and committed, and no `wl_buffer.release` yet.
    pub busy: bool,
    /// 0: contents unknown. n: holds the frame drawn n frames ago.
    pub age: u8,
    pub last_used: Instant,
    pub backing: T,
}

/// What [`SlotBook::choose`] found for the next frame.
#[derive(Debug, PartialEq, Eq)]
pub enum Choice {
    /// A free buffer of the right size and format.
    Reuse(u64),
    /// A free buffer of another size or format: replace its backing.
    Replace(u64),
    /// No free buffer and room for one more.
    Allocate,
    /// The compositor holds every buffer and there is no room for another.
    Busy,
}

#[derive(Debug)]
pub struct SlotBook<T> {
    slots: Vec<Slot<T>>,
    next_id: u64,
}

impl<T> Default for SlotBook<T> {
    fn default() -> Self {
        Self {
            slots: Vec::new(),
            next_id: 1,
        }
    }
}

impl<T> SlotBook<T> {
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn busy_count(&self) -> usize {
        self.slots.iter().filter(|s| s.busy).count()
    }

    pub fn iter(&self) -> impl Iterator<Item = &Slot<T>> {
        self.slots.iter()
    }

    pub fn get(&self, id: u64) -> Option<&Slot<T>> {
        self.slots.iter().find(|s| s.id == id)
    }

    pub fn get_mut(&mut self, id: u64) -> Option<&mut Slot<T>> {
        self.slots.iter_mut().find(|s| s.id == id)
    }

    /// Pick the buffer the next frame draws into. A free buffer whose
    /// contents are newest wins, so a compositor that releases promptly
    /// keeps the window on one buffer, repainting only the damage.
    pub fn choose(&self, width: u32, height: u32, format: PixelFormat) -> Choice {
        let free = || self.slots.iter().filter(|s| !s.busy);
        let matching = free()
            .filter(|s| s.width == width && s.height == height && s.format == format)
            .min_by_key(|s| {
                if s.age == 0 {
                    u16::MAX
                } else {
                    u16::from(s.age)
                }
            });
        if let Some(slot) = matching {
            return Choice::Reuse(slot.id);
        }
        if let Some(slot) = free().next() {
            return Choice::Replace(slot.id);
        }
        if self.slots.len() < MAX_BUFFERS {
            Choice::Allocate
        } else {
            Choice::Busy
        }
    }

    /// The id the next [`Self::insert`] assigns.
    pub fn peek_next_id(&self) -> u64 {
        self.next_id
    }

    /// Add a buffer with unknown contents and return its id.
    pub fn insert(
        &mut self,
        width: u32,
        height: u32,
        format: PixelFormat,
        now: Instant,
        backing: T,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        self.slots.push(Slot {
            id,
            width,
            height,
            format,
            busy: false,
            age: 0,
            last_used: now,
            backing,
        });
        id
    }

    pub fn remove(&mut self, id: u64) -> Option<Slot<T>> {
        let index = self.slots.iter().position(|s| s.id == id)?;
        Some(self.slots.swap_remove(index))
    }

    /// The frame in `id` was drawn: it is now the newest (age 1) and every
    /// other known buffer is one frame older. Ages past what the damage
    /// history covers become unknown.
    pub fn drew(&mut self, id: u64, now: Instant, history_frames: usize) {
        for slot in &mut self.slots {
            if slot.id == id {
                slot.age = 1;
                slot.last_used = now;
            } else if slot.age != 0 {
                slot.age = slot.age.saturating_add(1);
                if usize::from(slot.age) > history_frames + 1 {
                    slot.age = 0;
                }
            }
        }
    }

    /// `id` was attached and committed.
    pub fn committed(&mut self, id: u64) {
        if let Some(slot) = self.get_mut(id) {
            slot.busy = true;
        }
    }

    /// The compositor sent `wl_buffer.release` for `id`.
    pub fn released(&mut self, id: u64) {
        if let Some(slot) = self.get_mut(id) {
            slot.busy = false;
        }
    }

    /// The renderer took `id` and gave it back without presenting: what it
    /// holds now is unknown.
    pub fn abandoned(&mut self, id: u64) {
        if let Some(slot) = self.get_mut(id) {
            slot.age = 0;
        }
    }

    /// Every buffer's contents are unknown (a resize or format change).
    pub fn invalidate(&mut self) {
        for slot in &mut self.slots {
            slot.age = 0;
        }
    }

    /// The id of the buffer holding the newest frame, if any does.
    pub fn newest(&self) -> Option<u64> {
        self.slots.iter().find(|s| s.age == 1).map(|s| s.id)
    }

    /// Free buffers that can go now. With `keep_newest`, the buffer holding
    /// the newest frame stays so the next frame repaints only its damage;
    /// without it (a hidden window) every free buffer goes. A buffer the
    /// compositor holds is never listed.
    pub fn spares(&self, keep_newest: bool) -> Vec<u64> {
        let keep = if keep_newest { self.newest() } else { None };
        self.slots
            .iter()
            .filter(|s| !s.busy && Some(s.id) != keep)
            .map(|s| s.id)
            .collect()
    }

    /// Spares unused for [`IDLE_RELEASE`] at `now`.
    pub fn idle_spares(&self, now: Instant) -> Vec<u64> {
        let keep = self.newest();
        self.slots
            .iter()
            .filter(|s| {
                !s.busy
                    && Some(s.id) != keep
                    && now.saturating_duration_since(s.last_used) >= IDLE_RELEASE
            })
            .map(|s| s.id)
            .collect()
    }

    /// When the next free spare becomes idle: [`IDLE_RELEASE`] after its
    /// last use, which may have passed (it can go now). A spare the
    /// compositor holds has no deadline here, since it cannot go yet; see
    /// [`Self::held_spare`]. With `keep_newest` the buffer holding the
    /// newest frame is not a spare.
    pub fn idle_deadline(&self, keep_newest: bool) -> Option<Instant> {
        let keep = if keep_newest { self.newest() } else { None };
        self.slots
            .iter()
            .filter(|s| !s.busy && Some(s.id) != keep)
            .map(|s| s.last_used + IDLE_RELEASE)
            .min()
    }

    /// A spare the compositor still holds, last used within
    /// [`BUSY_RECHECK`] of `now`. Its release wakes nobody, so the
    /// presenter has its caller check again.
    pub fn held_spare(&self, now: Instant, keep_newest: bool) -> bool {
        let keep = if keep_newest { self.newest() } else { None };
        self.slots.iter().any(|s| {
            s.busy
                && Some(s.id) != keep
                && now.saturating_duration_since(s.last_used) < BUSY_RECHECK
        })
    }

    /// Buffers of a size or format no frame will use again. Free ones can go
    /// now; busy ones go when the compositor releases them.
    pub fn stale(&self, width: u32, height: u32, format: PixelFormat) -> Vec<u64> {
        self.slots
            .iter()
            .filter(|s| !s.busy && (s.width, s.height, s.format) != (width, height, format))
            .map(|s| s.id)
            .collect()
    }
}

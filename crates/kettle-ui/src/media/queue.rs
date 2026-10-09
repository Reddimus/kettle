//! The render queue for `show` and for what the user asks to preview: one
//! render at a time, a few waiting, and fair between senders.
//!
//! A sender is a verified caller's pane, or else the sending process (its
//! pid and start time), never a connection: reconnecting buys nothing. Each
//! sender has at most one push waiting; a newer push from the same sender
//! takes the waiting one's place in line, and the one it displaces is
//! answered as busy. Every push has a deadline from its admission; one still
//! waiting when it passes is answered as busy too, having never started.
//!
//! The user is a sender of their own with a slot pushes never fill: what
//! the user last asked for waits there, ahead of every push, and a newer
//! request takes its place.

use std::collections::VecDeque;
use std::time::Instant;

use kettle_ctl::process::ProcessIdentity;

/// Pushes that may wait behind the render in progress. A fourth slot is kept
/// for work the user starts ([`Sender::User`]).
pub(crate) const MAX_QUEUED_PUSHES: usize = 3;

/// Who a push counts against.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Sender {
    /// A caller verified to run in this pane.
    Pane(u64),
    /// A caller Kettle could not place, by the kernel's word for it.
    Process(ProcessIdentity),
    /// The user, previewing something themselves.
    User,
    /// A pane's lane rendering its item again: one at a time, and a newer
    /// one takes the waiting one's place.
    Lane(u64),
}

/// What admitting a push did with it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Admitted<T> {
    Queued,
    /// It took the place of this sender's waiting push, returned here to
    /// be answered as busy.
    Displaced(T),
    /// No room: the push itself comes back.
    Busy(T),
}

struct Waiting<T> {
    sender: Sender,
    deadline: Instant,
    work: T,
}

pub(crate) struct Queue<T> {
    waiting: VecDeque<Waiting<T>>,
    /// What the user last asked for, while it waits.
    user: Option<Waiting<T>>,
    /// Whether a render is in progress. Its work is with the renderer.
    rendering: bool,
}

impl<T> Default for Queue<T> {
    fn default() -> Self {
        Self {
            waiting: VecDeque::new(),
            user: None,
            rendering: false,
        }
    }
}

impl<T> Queue<T> {
    pub(crate) fn admit(&mut self, sender: Sender, deadline: Instant, work: T) -> Admitted<T> {
        if sender == Sender::User {
            let waiting = Waiting {
                sender,
                deadline,
                work,
            };
            return match self.user.replace(waiting) {
                Some(displaced) => Admitted::Displaced(displaced.work),
                None => Admitted::Queued,
            };
        }
        if let Some(slot) = self.waiting.iter_mut().find(|slot| slot.sender == sender) {
            slot.deadline = deadline;
            return Admitted::Displaced(std::mem::replace(&mut slot.work, work));
        }
        if self.waiting.len() >= MAX_QUEUED_PUSHES {
            return Admitted::Busy(work);
        }
        self.waiting.push_back(Waiting {
            sender,
            deadline,
            work,
        });
        Admitted::Queued
    }

    /// Pushes whose deadline passed while they waited, to be answered busy.
    pub(crate) fn expire(&mut self, now: Instant) -> Vec<T> {
        let mut expired = Vec::new();
        if self.user.as_ref().is_some_and(|slot| slot.deadline <= now) {
            expired.extend(self.user.take().map(|slot| slot.work));
        }
        let mut kept = VecDeque::with_capacity(self.waiting.len());
        for slot in self.waiting.drain(..) {
            if slot.deadline <= now {
                expired.push(slot.work);
            } else {
                kept.push_back(slot);
            }
        }
        self.waiting = kept;
        expired
    }

    /// The next push to render, once nothing is rendering. The caller hands
    /// it to the renderer and calls [`Queue::finished`] when that ends.
    pub(crate) fn start_next(&mut self, now: Instant) -> Option<T> {
        if self.rendering {
            return None;
        }
        if self.user.as_ref().is_some_and(|slot| slot.deadline > now) {
            self.rendering = true;
            return self.user.take().map(|slot| slot.work);
        }
        let index = self.waiting.iter().position(|slot| slot.deadline > now)?;
        let slot = self.waiting.remove(index)?;
        self.rendering = true;
        Some(slot.work)
    }

    /// The render in progress ended, its worker cleaned up.
    pub(crate) fn finished(&mut self) {
        self.rendering = false;
    }

    pub(crate) fn is_rendering(&self) -> bool {
        self.rendering
    }

    /// Remove every waiting push `cancelled` picks, in order.
    pub(crate) fn cancel(&mut self, mut cancelled: impl FnMut(&T) -> bool) -> Vec<T> {
        let mut removed = Vec::new();
        if self.user.as_ref().is_some_and(|slot| cancelled(&slot.work)) {
            removed.extend(self.user.take().map(|slot| slot.work));
        }
        let mut kept = VecDeque::with_capacity(self.waiting.len());
        for slot in self.waiting.drain(..) {
            if cancelled(&slot.work) {
                removed.push(slot.work);
            } else {
                kept.push_back(slot);
            }
        }
        self.waiting = kept;
        removed
    }

    /// The soonest a waiting push expires.
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        self.waiting
            .iter()
            .chain(&self.user)
            .map(|slot| slot.deadline)
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn later(seconds: u64) -> Instant {
        Instant::now() + Duration::from_secs(seconds)
    }

    #[test]
    fn one_render_at_a_time_in_arrival_order() {
        let mut queue = Queue::default();
        let now = Instant::now();
        assert_eq!(
            queue.admit(Sender::Pane(1), later(15), 'a'),
            Admitted::Queued
        );
        assert_eq!(
            queue.admit(Sender::Pane(2), later(15), 'b'),
            Admitted::Queued
        );
        assert_eq!(queue.start_next(now), Some('a'));
        assert!(queue.is_rendering());
        assert_eq!(queue.start_next(now), None);
        queue.finished();
        assert_eq!(queue.start_next(now), Some('b'));
        queue.finished();
        assert_eq!(queue.start_next(now), None);
    }

    #[test]
    fn a_sender_has_one_waiting_push_and_the_newest_keeps_its_place() {
        let mut queue = Queue::default();
        let now = Instant::now();
        queue.admit(Sender::Pane(1), later(15), 'a');
        queue.admit(Sender::Pane(2), later(15), 'b');
        assert_eq!(
            queue.admit(Sender::Pane(1), later(15), 'c'),
            Admitted::Displaced('a')
        );
        assert_eq!(queue.start_next(now), Some('c'));
        // Rendering does not count as waiting: the same sender may queue one.
        assert_eq!(
            queue.admit(Sender::Pane(1), later(15), 'd'),
            Admitted::Queued
        );
        queue.finished();
        assert_eq!(queue.start_next(now), Some('b'));
    }

    #[test]
    fn senders_are_panes_or_processes_never_connections() {
        let mut queue = Queue::default();
        let process = ProcessIdentity::new_for_tests(40, 7);
        let reused_pid = ProcessIdentity::new_for_tests(40, 8);
        assert_eq!(
            queue.admit(Sender::Process(process), later(15), 1),
            Admitted::Queued
        );
        assert_eq!(
            queue.admit(Sender::Process(process), later(15), 2),
            Admitted::Displaced(1)
        );
        assert_eq!(
            queue.admit(Sender::Process(reused_pid), later(15), 3),
            Admitted::Queued
        );
        assert_eq!(
            queue.admit(Sender::Pane(40), later(15), 4),
            Admitted::Queued
        );
    }

    #[test]
    fn a_full_queue_is_busy() {
        let mut queue = Queue::default();
        for pane in 0..MAX_QUEUED_PUSHES as u64 {
            assert_eq!(
                queue.admit(Sender::Pane(pane), later(15), pane),
                Admitted::Queued
            );
        }
        assert_eq!(
            queue.admit(Sender::Pane(99), later(15), 99),
            Admitted::Busy(99)
        );
        // A sender already waiting still replaces its own push.
        assert_eq!(
            queue.admit(Sender::Pane(0), later(15), 100),
            Admitted::Displaced(0)
        );
    }

    #[test]
    fn expired_pushes_never_start() {
        let mut queue = Queue::default();
        let now = Instant::now();
        queue.admit(Sender::Pane(1), now, 'a');
        queue.admit(Sender::Pane(2), later(15), 'b');
        queue.admit(Sender::Pane(3), now + Duration::from_secs(1), 'c');
        assert_eq!(queue.next_deadline(), Some(now));
        // Starting skips a push already out of time; expiry hands it back.
        assert_eq!(queue.start_next(now), Some('b'));
        assert_eq!(queue.expire(now), vec!['a']);
        assert_eq!(queue.expire(now + Duration::from_secs(1)), vec!['c']);
        assert_eq!(queue.next_deadline(), None);
    }

    /// The user's request waits in a slot of its own, even with every push
    /// slot full, runs before any push, and a newer one takes its place.
    #[test]
    fn the_users_request_waits_apart_and_runs_first() {
        let mut queue = Queue::default();
        let now = Instant::now();
        for pane in 0..MAX_QUEUED_PUSHES as u64 {
            queue.admit(Sender::Pane(pane), later(15), pane);
        }
        assert_eq!(
            queue.admit(Sender::Pane(9), later(15), 9),
            Admitted::Busy(9)
        );
        assert_eq!(queue.admit(Sender::User, later(15), 100), Admitted::Queued);
        assert_eq!(
            queue.admit(Sender::User, later(15), 101),
            Admitted::Displaced(100)
        );
        assert_eq!(queue.start_next(now), Some(101), "the user goes first");
        queue.finished();
        assert_eq!(queue.start_next(now), Some(0));
        queue.finished();
        // Expiry and cancellation reach the user's slot too.
        queue.admit(Sender::User, now, 102);
        assert_eq!(queue.next_deadline(), Some(now));
        assert_eq!(queue.expire(now), vec![102]);
        queue.admit(Sender::User, later(15), 103);
        assert_eq!(queue.cancel(|work| *work == 103), vec![103]);
        assert_eq!(queue.start_next(now), Some(1));
    }

    #[test]
    fn cancelling_removes_only_what_it_picks() {
        let mut queue = Queue::default();
        for pane in 1..=3 {
            queue.admit(Sender::Pane(pane), later(15), pane);
        }
        assert_eq!(queue.cancel(|pane| pane % 2 == 1), vec![1, 3]);
        assert_eq!(queue.start_next(Instant::now()), Some(2));
    }
}

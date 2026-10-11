//! The mouse buttons held down in a window, and whose gesture each is: a
//! program's, when its pane received the press, or Kettle's own (a
//! selection, a tab drag, a card). The newest press owns the drag. Each
//! press a program received gets exactly one release, in the pane it went
//! to: wherever keyboard focus has gone since, and, when Kettle ends the
//! gesture early, at the last cell that pane was told of. Its input keeps
//! room for releases, so a full queue cannot refuse one.

/// A held button.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Held {
    pub button: u8,
    /// The pane whose program received the press, or `None` for a gesture
    /// Kettle handles itself.
    pub pane: Option<u64>,
    /// The cell last reported to that pane, as (row, column).
    pub cell: (usize, usize),
}

/// At most one entry per button, newest last, so the list never outgrows
/// the buttons a mouse has.
#[derive(Debug, Default)]
pub(crate) struct HeldButtons {
    held: Vec<Held>,
}

impl HeldButtons {
    /// `button` went down: its press reached `pane`'s program at `cell`, or
    /// Kettle handles it (`None`). Returns the button's earlier entry, whose
    /// release never came, so its program can be sent one.
    pub(crate) fn pressed(
        &mut self,
        button: u8,
        pane: Option<u64>,
        cell: (usize, usize),
    ) -> Option<Held> {
        let stale = self.take(button);
        self.held.push(Held { button, pane, cell });
        stale
    }

    /// End `button`'s entry, returning it.
    pub(crate) fn take(&mut self, button: u8) -> Option<Held> {
        let at = self.held.iter().position(|held| held.button == button)?;
        Some(self.held.remove(at))
    }

    /// End `button`'s gesture if it is Kettle's own, whatever handles its
    /// release; a program's stays for its release to take.
    pub(crate) fn end_own(&mut self, button: u8) {
        self.held
            .retain(|held| held.button != button || held.pane.is_some());
    }

    /// The program gesture a drag reports to, if the newest press still
    /// held is a program's; a newer gesture of Kettle's own takes the drag.
    pub(crate) fn dragging(&self) -> Option<Held> {
        self.held.last().filter(|held| held.pane.is_some()).copied()
    }

    /// Note that `pane` was told of `cell`.
    pub(crate) fn told(&mut self, pane: u64, cell: (usize, usize)) {
        for held in self.held.iter_mut().filter(|held| held.pane == Some(pane)) {
            held.cell = cell;
        }
    }

    /// Whether a program holds a press with its release still to come.
    pub(crate) fn any(&self) -> bool {
        self.held.iter().any(|held| held.pane.is_some())
    }

    /// End every entry `which` names, for Kettle to release them early.
    pub(crate) fn take_where(&mut self, mut which: impl FnMut(&Held) -> bool) -> Vec<Held> {
        let mut taken = Vec::new();
        self.held.retain(|held| {
            if which(held) {
                taken.push(*held);
                false
            } else {
                true
            }
        });
        taken
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each button has one entry and the newest press drags; a release
    /// takes its own button's entry once, and a press after a lost release
    /// hands back the stale entry for its pane to be released.
    #[test]
    fn each_held_button_keeps_its_own_pane_until_its_release() {
        let mut held = HeldButtons::default();
        assert_eq!(held.dragging(), None);
        assert_eq!(held.pressed(0, Some(7), (1, 2)), None);
        assert_eq!(held.pressed(1, Some(9), (3, 4)), None);
        assert!(held.any());
        assert_eq!(
            held.dragging().map(|held| (held.button, held.pane)),
            Some((1, Some(9)))
        );
        held.told(7, (5, 6));
        assert_eq!(held.take(1).map(|held| held.pane), Some(Some(9)));
        assert_eq!(held.take(1), None, "once");
        assert_eq!(
            held.dragging(),
            Some(Held {
                button: 0,
                pane: Some(7),
                cell: (5, 6),
            }),
            "the left drag resumes after the middle release"
        );
        // The left release never came; the next left press returns its entry.
        assert_eq!(
            held.pressed(0, Some(8), (0, 0)).map(|held| held.pane),
            Some(Some(7))
        );
        held.pressed(128, Some(8), (0, 0));
        let early = held.take_where(|held| held.pane == Some(8));
        assert_eq!(early.len(), 2);
        assert!(!held.any());
    }

    /// A newer gesture of Kettle's own takes the drag from a program's held
    /// button until it ends, however its release is handled, and a program's
    /// gesture is never ended that way.
    #[test]
    fn the_newest_press_owns_the_drag() {
        let mut held = HeldButtons::default();
        held.pressed(128, Some(7), (1, 1));
        held.pressed(0, None, (0, 0));
        assert_eq!(held.dragging(), None, "Kettle's selection drags");
        assert!(held.any(), "the program still holds Back");
        held.end_own(0);
        assert_eq!(held.dragging().map(|held| held.button), Some(128));
        held.end_own(128);
        assert_eq!(held.take(128).map(|held| held.cell), Some((1, 1)));
        assert!(!held.any());
    }
}

//! When the tab strip shows its go-to-tab shortcuts.
//!
//! At rest a horizontal tab shows only its title. Two transient pills teach
//! the shortcuts instead:
//!
//! - [`NewTabHintFade`]: a tab just opened from New Tab, `+` or the new-tab
//!   menu shows its full chord (`Cmd+3`, `Alt+3`) for [`NEW_TAB_HINT_HOLD`],
//!   then fades out over [`NEW_TAB_HINT_FADE`]. Reduced motion skips it.
//! - [`TabHintPeek`]: holding exactly the go-to-tab modifiers for
//!   [`TAB_HINT_PEEK_DELAY`] shows each of the first nine tabs' digit until
//!   they are released or another key is pressed.
//!
//! Neither holds a timer. The scheduler wakes once at the end of the hold
//! or the peek delay, and only the fade itself asks for frames, one every
//! [`NEW_TAB_HINT_FRAME`].

use kettle_config::keybinds::{Action, Bindings, Key, Mods, Trigger};
use std::time::{Duration, Instant};

/// How long a new tab's chord shows at full opacity.
pub(crate) const NEW_TAB_HINT_HOLD: Duration = Duration::from_millis(1600);

/// How long it then takes to fade out.
pub(crate) const NEW_TAB_HINT_FADE: Duration = Duration::from_millis(400);

/// The fade's frame interval, about 60 fps.
pub(crate) const NEW_TAB_HINT_FRAME: Duration = Duration::from_millis(16);

/// How long the go-to-tab modifiers are held before the digits show, so a
/// quick shortcut such as Cmd+C never flashes them.
pub(crate) const TAB_HINT_PEEK_DELAY: Duration = Duration::from_millis(150);

/// The modifiers that, with `1`..`9`, go to the first nine tabs, when one
/// set does for all nine: Cmd on macOS and Alt elsewhere by default, or the
/// user's own. `None` when the bindings name no such set, so there is no
/// digit to teach; a set of Shift alone, which typing holds, never counts.
pub(crate) fn tab_goto_peek_modifiers(bindings: &Bindings) -> Option<Mods> {
    let first = kettle_config::keybinds::hint_trigger(
        bindings,
        &Action::GotoTab(0),
        cfg!(target_os = "macos"),
    )?;
    let mods = first.mods;
    if !mods.intersects(Mods::CTRL | Mods::ALT | Mods::SUPER) {
        return None;
    }
    (0..9u8)
        .all(|index| {
            let digit = char::from(b'1' + index);
            bindings.get(&Trigger::new(mods, Key::Char(digit))) == Some(&Action::GotoTab(index))
        })
        .then_some(mods)
}

/// The tab a new-tab hint names, and since when.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct NewTabHint {
    /// Index of the new tab.
    tab: usize,
    /// How many tabs the window had once it was open.
    tabs: usize,
    /// A pane of the new tab, which finds it again after a reorder.
    pane: u64,
    started: Instant,
    /// When the fade's next frame is due.
    next_frame: Option<Instant>,
}

/// A new tab's go-to-tab chord, shown briefly when it opens.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct NewTabHintFade {
    armed: Option<NewTabHint>,
}

impl NewTabHintFade {
    /// Tab `tab` of `tabs`, holding `pane`, just opened at `now`. Only tabs
    /// 2 to 9 have a chord worth teaching (the first is the only one when it
    /// opens), and reduced motion shows none. Says whether it armed.
    pub(crate) fn arm(
        &mut self,
        tab: usize,
        tabs: usize,
        pane: u64,
        reduced_motion: bool,
        now: Instant,
    ) -> bool {
        self.armed =
            (!reduced_motion && (1..9).contains(&tab) && tab < tabs).then_some(NewTabHint {
                tab,
                tabs,
                pane,
                started: now,
                next_frame: None,
            });
        self.armed.is_some()
    }

    /// Drop the hint. Says whether one was showing, so the frame needs
    /// repainting.
    pub(crate) fn cancel(&mut self) -> bool {
        self.armed.take().is_some()
    }

    pub(crate) fn is_armed(&self) -> bool {
        self.armed.is_some()
    }

    /// The tab the hint names.
    pub(crate) fn tab(&self) -> Option<usize> {
        self.armed.map(|hint| hint.tab)
    }

    /// The hint still names the tab it was armed for: that tab is still the
    /// active one, at the same place, in a strip of as many tabs. A switch,
    /// a close, an open or a reorder that moves it ends it.
    pub(crate) fn still_names(
        &self,
        active: usize,
        tabs: usize,
        holds_pane: impl Fn(usize, u64) -> bool,
    ) -> bool {
        self.armed.is_some_and(|hint| {
            hint.tab == active && hint.tabs == tabs && holds_pane(hint.tab, hint.pane)
        })
    }

    /// The hint's opacity at `now`: full through the hold, falling linearly
    /// to nothing over the fade. `None` when there is none or it is over.
    pub(crate) fn alpha(&self, now: Instant) -> Option<f32> {
        let hint = self.armed?;
        let elapsed = now.saturating_duration_since(hint.started);
        if elapsed < NEW_TAB_HINT_HOLD {
            Some(1.0)
        } else if elapsed < NEW_TAB_HINT_HOLD + NEW_TAB_HINT_FADE {
            let into = (elapsed - NEW_TAB_HINT_HOLD).as_secs_f32();
            Some((1.0 - into / NEW_TAB_HINT_FADE.as_secs_f32()).clamp(0.0, 1.0))
        } else {
            None
        }
    }

    /// Advance to `now`. Returns whether a frame is due (a fade step, or
    /// the frame that erases a finished hint) and when to wake next: once
    /// at the end of the hold, every [`NEW_TAB_HINT_FRAME`] through the
    /// fade, and never after it.
    pub(crate) fn poll(&mut self, now: Instant) -> (bool, Option<Instant>) {
        let Some(hint) = self.armed.as_mut() else {
            return (false, None);
        };
        let fade_at = hint.started + NEW_TAB_HINT_HOLD;
        let end = fade_at + NEW_TAB_HINT_FADE;
        if now >= end {
            self.armed = None;
            return (true, None);
        }
        if now < fade_at {
            // The open that armed it drew the first frame.
            return (false, Some(fade_at));
        }
        let due = hint.next_frame.is_none_or(|at| now >= at);
        if due {
            hint.next_frame = Some(now + NEW_TAB_HINT_FRAME);
        }
        let wake = hint.next_frame.map_or(end, |at| at.min(end));
        (due, Some(wake))
    }
}

/// Whether the go-to-tab digits show while their modifiers are held.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum TabHintPeek {
    #[default]
    Idle,
    /// The modifiers went down at this instant; the digits show once
    /// [`TAB_HINT_PEEK_DELAY`] has passed with nothing else pressed.
    Armed(Instant),
    Visible,
}

impl TabHintPeek {
    /// The modifiers held changed to `held` at `now`. Exactly the `peek`
    /// set, where the window `allows` a peek, arms it; anything else ends
    /// it. Staying on the set (a second Cmd key, a repeat) keeps its time.
    /// Says whether the digits went away, so the frame needs repainting.
    pub(crate) fn modifiers_changed(
        &mut self,
        held: Mods,
        peek: Option<Mods>,
        allows: bool,
        now: Instant,
    ) -> bool {
        if allows && peek == Some(held) && !held.is_empty() {
            if *self == Self::Idle {
                *self = Self::Armed(now);
            }
            false
        } else {
            self.clear()
        }
    }

    /// End the peek: a key pressed, focus lost, a modal opened. Says
    /// whether the digits were showing.
    pub(crate) fn clear(&mut self) -> bool {
        std::mem::take(self) == Self::Visible
    }

    pub(crate) fn visible(&self) -> bool {
        *self == Self::Visible
    }

    /// Advance to `now`. Returns whether the digits just came to show, so
    /// the frame needs painting, and when to wake for the delay's end.
    pub(crate) fn poll(&mut self, now: Instant) -> (bool, Option<Instant>) {
        match *self {
            Self::Armed(since) if now >= since + TAB_HINT_PEEK_DELAY => {
                *self = Self::Visible;
                (true, None)
            }
            Self::Armed(since) => (false, Some(since + TAB_HINT_PEEK_DELAY)),
            Self::Idle | Self::Visible => (false, None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn a_new_tab_holds_its_chord_then_fades_it_out() {
        let t0 = Instant::now();
        let mut fade = NewTabHintFade::default();
        assert!(fade.arm(2, 3, 7, false, t0));
        assert_eq!(fade.tab(), Some(2));
        assert_eq!(fade.alpha(t0), Some(1.0));
        assert_eq!(fade.alpha(t0 + ms(1599)), Some(1.0));
        let half = fade.alpha(t0 + ms(1800)).unwrap();
        assert!((half - 0.5).abs() < 0.01, "{half}");
        assert!(fade.alpha(t0 + ms(1999)).unwrap() < 0.01);
        assert_eq!(fade.alpha(t0 + ms(2000)), None);
        assert_eq!(NEW_TAB_HINT_HOLD + NEW_TAB_HINT_FADE, ms(2000));
    }

    #[test]
    fn the_fade_wakes_once_for_the_hold_then_steps_frames_and_stops() {
        let t0 = Instant::now();
        let mut fade = NewTabHintFade::default();
        fade.arm(1, 2, 7, false, t0);
        // The hold: one wake, at its end, and no frames.
        assert_eq!(fade.poll(t0 + ms(5)), (false, Some(t0 + ms(1600))));
        assert_eq!(fade.poll(t0 + ms(900)), (false, Some(t0 + ms(1600))));
        // The fade: a frame, then a wake one frame later.
        assert_eq!(fade.poll(t0 + ms(1600)), (true, Some(t0 + ms(1616))));
        // An early turn of the loop paints nothing new.
        assert_eq!(fade.poll(t0 + ms(1605)), (false, Some(t0 + ms(1616))));
        assert_eq!(fade.poll(t0 + ms(1616)), (true, Some(t0 + ms(1632))));
        // The last wake lands on the end, which erases it and stops.
        assert_eq!(fade.poll(t0 + ms(1990)), (true, Some(t0 + ms(2000))));
        assert_eq!(fade.poll(t0 + ms(2000)), (true, None));
        assert!(!fade.is_armed());
        assert_eq!(fade.poll(t0 + ms(2100)), (false, None));
    }

    #[test]
    fn reduced_motion_and_tabs_without_a_chord_show_no_hint() {
        let t0 = Instant::now();
        let mut fade = NewTabHintFade::default();
        assert!(!fade.arm(2, 3, 7, true, t0), "reduced motion");
        assert!(!fade.arm(0, 1, 7, false, t0), "the first tab");
        assert!(!fade.arm(9, 10, 7, false, t0), "the tenth tab");
        assert!(fade.arm(8, 9, 7, false, t0), "the ninth tab");
        // Arming for a tab with no chord drops the one before.
        assert!(!fade.arm(9, 10, 7, false, t0));
        assert_eq!(fade.alpha(t0), None);
    }

    #[test]
    fn the_hint_ends_once_it_no_longer_names_its_tab() {
        let t0 = Instant::now();
        let mut fade = NewTabHintFade::default();
        fade.arm(2, 3, 7, false, t0);
        let holds = |tab: usize, pane: u64| tab == 2 && pane == 7;
        assert!(fade.still_names(2, 3, holds));
        assert!(!fade.still_names(1, 3, holds), "switched away");
        assert!(!fade.still_names(2, 2, holds), "a tab closed");
        assert!(!fade.still_names(2, 4, holds), "a tab opened");
        assert!(!fade.still_names(2, 3, |_, _| false), "reordered");
        assert!(fade.cancel());
        assert!(!fade.cancel());
        assert!(!fade.still_names(2, 3, holds));
    }

    #[test]
    fn peek_shows_after_the_delay_and_ends_on_release_or_a_key() {
        let t0 = Instant::now();
        let cmd = Some(Mods::SUPER);
        let mut peek = TabHintPeek::default();
        assert!(!peek.modifiers_changed(Mods::SUPER, cmd, true, t0));
        assert_eq!(peek, TabHintPeek::Armed(t0));
        assert_eq!(peek.poll(t0 + ms(100)), (false, Some(t0 + ms(150))));
        assert!(!peek.visible());
        // A second report of the same set keeps the first time.
        peek.modifiers_changed(Mods::SUPER, cmd, true, t0 + ms(120));
        assert_eq!(peek.poll(t0 + ms(150)), (true, None));
        assert!(peek.visible());
        assert_eq!(peek.poll(t0 + ms(400)), (false, None));
        // Release.
        assert!(peek.modifiers_changed(Mods::empty(), cmd, true, t0 + ms(500)));
        assert_eq!(peek, TabHintPeek::Idle);
        // A key pressed before the delay never shows it.
        peek.modifiers_changed(Mods::SUPER, cmd, true, t0);
        assert!(!peek.clear());
        assert_eq!(peek.poll(t0 + ms(200)), (false, None));
    }

    #[test]
    fn peek_needs_exactly_the_set_and_a_window_that_allows_it() {
        let t0 = Instant::now();
        let mut peek = TabHintPeek::default();
        peek.modifiers_changed(Mods::SUPER | Mods::SHIFT, Some(Mods::SUPER), true, t0);
        assert_eq!(peek, TabHintPeek::Idle, "another chord");
        peek.modifiers_changed(Mods::SUPER, None, true, t0);
        assert_eq!(peek, TabHintPeek::Idle, "nothing to teach");
        // Unfocused, a modal, one tab, or a link under the pointer.
        peek.modifiers_changed(Mods::SUPER, Some(Mods::SUPER), false, t0);
        assert_eq!(peek, TabHintPeek::Idle, "not allowed");
        peek.modifiers_changed(Mods::SUPER, Some(Mods::SUPER), true, t0);
        peek.poll(t0 + ms(150));
        assert!(peek.visible());
        // Shift added while it shows ends it.
        assert!(peek.modifiers_changed(
            Mods::SUPER | Mods::SHIFT,
            Some(Mods::SUPER),
            true,
            t0 + ms(200)
        ));
        // A link coming under the pointer ends it too.
        peek.modifiers_changed(Mods::SUPER, Some(Mods::SUPER), true, t0);
        peek.poll(t0 + ms(150));
        assert!(peek.modifiers_changed(Mods::SUPER, Some(Mods::SUPER), false, t0 + ms(300)));
    }

    #[test]
    fn the_peek_set_is_the_one_all_nine_digits_share() {
        let defaults = kettle_config::keybinds::defaults();
        let expected = if cfg!(target_os = "macos") {
            Mods::SUPER
        } else {
            Mods::ALT
        };
        assert_eq!(tab_goto_peek_modifiers(&defaults), Some(expected));
        // One digit moved elsewhere: no set covers all nine.
        let mut partial = defaults.clone();
        partial.retain(|_, action| *action != Action::GotoTab(4));
        partial.insert(Trigger::new(Mods::CTRL, Key::F(5)), Action::GotoTab(4));
        assert_eq!(tab_goto_peek_modifiers(&partial), None);
        // Unbound entirely.
        let mut none = defaults.clone();
        none.retain(|_, action| !matches!(action, Action::GotoTab(_)));
        assert_eq!(tab_goto_peek_modifiers(&none), None);
        // The user's own set.
        let mut ctrl = none.clone();
        for index in 0..9u8 {
            let digit = char::from(b'1' + index);
            ctrl.insert(
                Trigger::new(Mods::CTRL, Key::Char(digit)),
                Action::GotoTab(index),
            );
        }
        assert_eq!(tab_goto_peek_modifiers(&ctrl), Some(Mods::CTRL));
        // Shift alone is held for typing and never peeks.
        let mut shift = none;
        for index in 0..9u8 {
            let digit = char::from(b'1' + index);
            shift.insert(
                Trigger::new(Mods::SHIFT, Key::Char(digit)),
                Action::GotoTab(index),
            );
        }
        assert_eq!(tab_goto_peek_modifiers(&shift), None);
    }
}

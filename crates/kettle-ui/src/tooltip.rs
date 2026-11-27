//! When a chrome control's tooltip shows.
//!
//! The first tooltip waits until the pointer has rested on its control for
//! [`TOOLTIP_DELAY`]. Within [`TOOLTIP_WARM`] of a tooltip going away,
//! moving onto the next control shows its tooltip at once, so running the
//! pointer along a row of buttons reads each name without the wait. A press
//! hides the tooltip, and the control pressed shows none again until the
//! pointer leaves it and comes back. The clock holds no timer: the scheduler
//! wakes once at [`TooltipClock::poll`]'s deadline and nothing redraws
//! while it waits.

use std::time::{Duration, Instant};

/// How long the pointer rests on a control before its tooltip shows.
pub(crate) const TOOLTIP_DELAY: Duration = Duration::from_millis(500);

/// How long after a tooltip goes away the next one still shows at once.
pub(crate) const TOOLTIP_WARM: Duration = Duration::from_secs(1);

/// A control with a tooltip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TooltipTarget {
    /// The strip's New Tab button.
    NewTab,
    /// New Tab Options, or the GNOME header's main menu.
    NewTabMenu,
    /// The title bar's Settings gear.
    Settings,
    /// The GNOME header's Find button.
    Search,
    /// A Linux header bar's window button.
    WindowControl(kettle_render::WindowControl),
    /// The tab strip's back overflow arrow.
    ScrollBack,
    /// The tab strip's forward overflow arrow.
    ScrollForward,
    /// A tab whose title the strip shortened, by index.
    TabTitle(usize),
    /// A media viewer or preview lane's button, by pane.
    Media(u64, kettle_render::MediaLaneHit),
}

/// Where a control is, in physical pixels: x, y, width, height.
type Rect = (f32, f32, f32, f32);

/// The pointer's tooltip state in one window.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TooltipClock {
    /// The control under the pointer, and since when.
    hovered: Option<(TooltipTarget, Instant)>,
    /// Where the hovered control is.
    hovered_rect: Option<Rect>,
    /// The pointer came onto it while tooltips were warm.
    warm: bool,
    /// The control a press hid the tooltip of, and where it was: it shows
    /// none until the pointer has left that place.
    pressed: Option<(TooltipTarget, Rect)>,
    /// The scheduler has asked for the frame that shows it.
    revealed: bool,
    /// Until when the next control's tooltip shows at once.
    warm_until: Option<Instant>,
}

impl TooltipClock {
    /// The pointer, at `pointer` (`None` outside the window), is over
    /// `target` at `now`, a control and where it is, or over no control
    /// whose tooltip may show. Staying on the same control keeps its time.
    /// Says whether the tooltip shown changed, so the frame needs
    /// repainting.
    pub(crate) fn hover(
        &mut self,
        target: Option<(TooltipTarget, Rect)>,
        pointer: Option<(f32, f32)>,
        now: Instant,
    ) -> bool {
        let was = self.shown(now);
        // Only the pointer leaving a pressed control lets it show its
        // tooltip again. A moment with no control to name (the press held,
        // a menu it opened, the strip laid out again) is no leave.
        if let Some((_, rect)) = self.pressed
            && !pointer.is_some_and(|(x, y)| inside(rect, x, y))
        {
            self.pressed = None;
        }
        self.hovered_rect = target.map(|(_, rect)| rect);
        let target = target.map(|(target, _)| target);
        if self.hovered.map(|(current, _)| current) != target {
            if was.is_some() {
                self.warm_until = Some(now + TOOLTIP_WARM);
            }
            self.warm = self.warm_until.is_some_and(|until| now <= until);
            self.hovered = target.map(|target| (target, now));
            self.revealed = self.warm;
        }
        was != self.shown(now)
    }

    /// A press at `now` hides the tooltip; the control under the pointer
    /// shows none until the pointer leaves it. Tooltips are no longer warm.
    /// Says whether a tooltip was showing.
    pub(crate) fn press(&mut self, now: Instant) -> bool {
        let was = self.shown(now).is_some();
        if let (Some((target, _)), Some(rect)) = (self.hovered, self.hovered_rect) {
            self.pressed = Some((target, rect));
        }
        self.warm_until = None;
        was
    }

    /// Whether a press hid `target`'s tooltip and the pointer has not left
    /// it since.
    fn held_back(&self, target: TooltipTarget) -> bool {
        self.pressed.is_some_and(|(pressed, _)| pressed == target)
    }

    /// The control whose tooltip shows at `now`.
    pub(crate) fn shown(&self, now: Instant) -> Option<TooltipTarget> {
        let (target, since) = self.hovered?;
        let rested = now.saturating_duration_since(since) >= TOOLTIP_DELAY;
        (!self.held_back(target) && (self.warm || rested)).then_some(target)
    }

    /// For the scheduler at `now`: whether a tooltip came due since it last
    /// looked, so the frame that shows it must be drawn, and when to look
    /// again while one is still waiting.
    pub(crate) fn poll(&mut self, now: Instant) -> (bool, Option<Instant>) {
        let Some((target, since)) = self.hovered else {
            return (false, None);
        };
        if self.held_back(target) || self.revealed {
            return (false, None);
        }
        let due = since + TOOLTIP_DELAY;
        if now >= due {
            self.revealed = true;
            (true, None)
        } else {
            (false, Some(due))
        }
    }
}

fn inside((left, top, width, height): Rect, x: f32, y: f32) -> bool {
    x >= left && x < left + width && y >= top && y < top + height
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLUS: Option<TooltipTarget> = Some(TooltipTarget::NewTab);
    const MENU: Option<TooltipTarget> = Some(TooltipTarget::NewTabMenu);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Where each control is in these tests, and the pointer on it; with no
    /// control the pointer is off both.
    fn place(target: Option<TooltipTarget>) -> (Option<(TooltipTarget, Rect)>, (f32, f32)) {
        match target {
            Some(TooltipTarget::NewTab) => (
                Some((TooltipTarget::NewTab, (0.0, 0.0, 10.0, 10.0))),
                (5.0, 5.0),
            ),
            Some(target) => (Some((target, (20.0, 0.0, 10.0, 10.0))), (25.0, 5.0)),
            None => (None, (100.0, 100.0)),
        }
    }

    /// The pointer moves onto `target`, or off every control.
    fn hover(clock: &mut TooltipClock, target: Option<TooltipTarget>, now: Instant) -> bool {
        let (target, pointer) = place(target);
        clock.hover(target, Some(pointer), now)
    }

    #[test]
    fn the_first_tooltip_waits_for_the_pointer_to_rest() {
        let t0 = Instant::now();
        let mut clock = TooltipClock::default();
        assert!(!hover(&mut clock, PLUS, t0), "nothing shows on arrival");
        assert_eq!(clock.shown(t0), None);
        assert_eq!(clock.shown(t0 + ms(499)), None);
        assert_eq!(clock.shown(t0 + TOOLTIP_DELAY), PLUS);
        // Moving within the control keeps its time.
        assert!(!hover(&mut clock, PLUS, t0 + ms(300)));
        assert_eq!(clock.shown(t0 + TOOLTIP_DELAY), PLUS);
    }

    #[test]
    fn the_scheduler_wakes_once_at_the_deadline_and_never_before() {
        let t0 = Instant::now();
        let mut clock = TooltipClock::default();
        assert_eq!(clock.poll(t0), (false, None), "no control, no wake");
        hover(&mut clock, PLUS, t0);
        assert_eq!(clock.poll(t0 + ms(100)), (false, Some(t0 + TOOLTIP_DELAY)));
        assert_eq!(clock.poll(t0 + TOOLTIP_DELAY), (true, None));
        assert_eq!(
            clock.poll(t0 + ms(900)),
            (false, None),
            "a shown tooltip asks for no more frames"
        );
    }

    #[test]
    fn neighbours_show_at_once_while_warm_and_wait_again_after() {
        let t0 = Instant::now();
        let mut clock = TooltipClock::default();
        hover(&mut clock, PLUS, t0);
        let shown = t0 + ms(600);
        // Straight onto the next control: shows at once.
        assert!(hover(&mut clock, MENU, shown));
        assert_eq!(clock.shown(shown), MENU);
        assert_eq!(clock.poll(shown), (false, None));
        // Off every control, then onto one within the second: still warm.
        assert!(hover(&mut clock, None, shown + ms(100)));
        assert!(hover(&mut clock, PLUS, shown + ms(900)));
        assert_eq!(clock.shown(shown + ms(900)), PLUS);
        // Off again for longer than a second: the wait is back.
        hover(&mut clock, None, shown + ms(1000));
        assert!(!hover(&mut clock, MENU, shown + ms(2100)));
        assert_eq!(clock.shown(shown + ms(2100)), None);
        assert_eq!(clock.shown(shown + ms(2600)), MENU);
    }

    #[test]
    fn leaving_before_the_tooltip_shows_does_not_warm_the_next() {
        let t0 = Instant::now();
        let mut clock = TooltipClock::default();
        hover(&mut clock, PLUS, t0);
        assert!(!hover(&mut clock, MENU, t0 + ms(400)));
        assert_eq!(clock.shown(t0 + ms(800)), None);
        assert_eq!(clock.shown(t0 + ms(900)), MENU);
    }

    #[test]
    fn a_press_hides_the_tooltip_until_the_pointer_leaves_and_returns() {
        let t0 = Instant::now();
        let mut clock = TooltipClock::default();
        hover(&mut clock, PLUS, t0);
        let shown = t0 + ms(600);
        assert!(clock.press(shown), "it was showing");
        assert_eq!(clock.shown(shown), None);
        // Resting on the pressed control brings nothing back.
        assert!(!hover(&mut clock, PLUS, shown + ms(10)));
        assert_eq!(clock.shown(shown + ms(5000)), None);
        assert_eq!(clock.poll(shown + ms(5000)), (false, None));
        // A press ends the warm spell, so the next control waits.
        assert!(!hover(&mut clock, MENU, shown + ms(100)));
        assert_eq!(clock.shown(shown + ms(100)), None);
        assert_eq!(clock.shown(shown + ms(600)), MENU);
        // Back on the pressed control after leaving it: shown again.
        hover(&mut clock, None, shown + ms(700));
        hover(&mut clock, PLUS, shown + ms(3000));
        assert_eq!(clock.shown(shown + ms(3500)), PLUS);
    }

    #[test]
    fn a_press_while_waiting_cancels_the_pending_tooltip() {
        let t0 = Instant::now();
        let mut clock = TooltipClock::default();
        hover(&mut clock, PLUS, t0);
        assert!(!clock.press(t0 + ms(200)));
        assert_eq!(clock.shown(t0 + ms(900)), None);
        assert_eq!(clock.poll(t0 + ms(900)), (false, None));
    }

    /// N1 of the 6.0 live recheck: after a click on New Tab with the pointer
    /// still on it, the tooltip came back half a second later. The press
    /// gates the strip's tooltip while held, and a dropdown it opens does
    /// too, so the control under the pointer reads as none for a moment;
    /// that was taken for a leave. Only the pointer leaving the control's
    /// place lets its tooltip show again.
    #[test]
    fn a_moment_with_no_control_is_no_leave_after_a_press() {
        let t0 = Instant::now();
        let mut clock = TooltipClock::default();
        let (plus, on_plus) = place(PLUS);
        clock.hover(plus, Some(on_plus), t0);
        assert_eq!(clock.shown(t0 + ms(600)), PLUS);
        clock.press(t0 + ms(600));
        // Held, then released, then the strip laid out again: the pointer
        // never moved.
        clock.hover(None, Some(on_plus), t0 + ms(610));
        clock.hover(plus, Some(on_plus), t0 + ms(700));
        clock.hover(None, Some(on_plus), t0 + ms(710));
        clock.hover(plus, Some(on_plus), t0 + ms(720));
        assert_eq!(
            clock.shown(t0 + ms(5000)),
            None,
            "no tooltip while it stays"
        );
        assert_eq!(
            clock.poll(t0 + ms(5000)),
            (false, None),
            "and no wake for one"
        );
        // Moving within the control is no leave either.
        clock.hover(plus, Some((9.0, 9.0)), t0 + ms(5100));
        assert_eq!(clock.shown(t0 + ms(6000)), None);
        // Off it and back: shown after a rest.
        clock.hover(None, Some((50.0, 50.0)), t0 + ms(6100));
        clock.hover(plus, Some(on_plus), t0 + ms(8000));
        assert_eq!(clock.shown(t0 + ms(8000)), None);
        assert_eq!(clock.shown(t0 + ms(8500)), PLUS);
        // Leaving the window is a leave.
        clock.press(t0 + ms(8600));
        clock.hover(None, None, t0 + ms(8700));
        clock.hover(plus, Some(on_plus), t0 + ms(9000));
        assert_eq!(clock.shown(t0 + ms(9500)), PLUS);
    }
}

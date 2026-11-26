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

/// The pointer's tooltip state in one window.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct TooltipClock {
    /// The control under the pointer, and since when.
    hovered: Option<(TooltipTarget, Instant)>,
    /// The pointer came onto it while tooltips were warm.
    warm: bool,
    /// A press hid its tooltip until the pointer leaves.
    pressed: bool,
    /// The scheduler has asked for the frame that shows it.
    revealed: bool,
    /// Until when the next control's tooltip shows at once.
    warm_until: Option<Instant>,
}

impl TooltipClock {
    /// The pointer is over `target` at `now`, or over no control with a
    /// tooltip. Staying on the same control keeps its time. Says whether the
    /// tooltip shown changed, so the frame needs repainting.
    pub(crate) fn hover(&mut self, target: Option<TooltipTarget>, now: Instant) -> bool {
        if self.hovered.map(|(current, _)| current) == target {
            return false;
        }
        let was = self.shown(now);
        if was.is_some() {
            self.warm_until = Some(now + TOOLTIP_WARM);
        }
        self.warm = self.warm_until.is_some_and(|until| now <= until);
        self.hovered = target.map(|target| (target, now));
        self.pressed = false;
        self.revealed = self.warm;
        was != self.shown(now)
    }

    /// A press at `now` hides the tooltip; the control under the pointer
    /// shows none until the pointer leaves it. Tooltips are no longer warm.
    /// Says whether a tooltip was showing.
    pub(crate) fn press(&mut self, now: Instant) -> bool {
        let was = self.shown(now).is_some();
        self.pressed = self.hovered.is_some();
        self.warm_until = None;
        was
    }

    /// The control whose tooltip shows at `now`.
    pub(crate) fn shown(&self, now: Instant) -> Option<TooltipTarget> {
        let (target, since) = self.hovered?;
        let rested = now.saturating_duration_since(since) >= TOOLTIP_DELAY;
        (!self.pressed && (self.warm || rested)).then_some(target)
    }

    /// For the scheduler at `now`: whether a tooltip came due since it last
    /// looked, so the frame that shows it must be drawn, and when to look
    /// again while one is still waiting.
    pub(crate) fn poll(&mut self, now: Instant) -> (bool, Option<Instant>) {
        let Some((_, since)) = self.hovered else {
            return (false, None);
        };
        if self.pressed || self.revealed {
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

#[cfg(test)]
mod tests {
    use super::*;

    const PLUS: Option<TooltipTarget> = Some(TooltipTarget::NewTab);
    const MENU: Option<TooltipTarget> = Some(TooltipTarget::NewTabMenu);

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn the_first_tooltip_waits_for_the_pointer_to_rest() {
        let t0 = Instant::now();
        let mut clock = TooltipClock::default();
        assert!(!clock.hover(PLUS, t0), "nothing shows on arrival");
        assert_eq!(clock.shown(t0), None);
        assert_eq!(clock.shown(t0 + ms(499)), None);
        assert_eq!(clock.shown(t0 + TOOLTIP_DELAY), PLUS);
        // Moving within the control keeps its time.
        assert!(!clock.hover(PLUS, t0 + ms(300)));
        assert_eq!(clock.shown(t0 + TOOLTIP_DELAY), PLUS);
    }

    #[test]
    fn the_scheduler_wakes_once_at_the_deadline_and_never_before() {
        let t0 = Instant::now();
        let mut clock = TooltipClock::default();
        assert_eq!(clock.poll(t0), (false, None), "no control, no wake");
        clock.hover(PLUS, t0);
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
        clock.hover(PLUS, t0);
        let shown = t0 + ms(600);
        // Straight onto the next control: shows at once.
        assert!(clock.hover(MENU, shown));
        assert_eq!(clock.shown(shown), MENU);
        assert_eq!(clock.poll(shown), (false, None));
        // Off every control, then onto one within the second: still warm.
        assert!(clock.hover(None, shown + ms(100)));
        assert!(clock.hover(PLUS, shown + ms(900)));
        assert_eq!(clock.shown(shown + ms(900)), PLUS);
        // Off again for longer than a second: the wait is back.
        clock.hover(None, shown + ms(1000));
        assert!(!clock.hover(MENU, shown + ms(2100)));
        assert_eq!(clock.shown(shown + ms(2100)), None);
        assert_eq!(clock.shown(shown + ms(2600)), MENU);
    }

    #[test]
    fn leaving_before_the_tooltip_shows_does_not_warm_the_next() {
        let t0 = Instant::now();
        let mut clock = TooltipClock::default();
        clock.hover(PLUS, t0);
        assert!(!clock.hover(MENU, t0 + ms(400)));
        assert_eq!(clock.shown(t0 + ms(800)), None);
        assert_eq!(clock.shown(t0 + ms(900)), MENU);
    }

    #[test]
    fn a_press_hides_the_tooltip_until_the_pointer_leaves_and_returns() {
        let t0 = Instant::now();
        let mut clock = TooltipClock::default();
        clock.hover(PLUS, t0);
        let shown = t0 + ms(600);
        assert!(clock.press(shown), "it was showing");
        assert_eq!(clock.shown(shown), None);
        // Resting on the pressed control brings nothing back.
        assert!(!clock.hover(PLUS, shown + ms(10)));
        assert_eq!(clock.shown(shown + ms(5000)), None);
        assert_eq!(clock.poll(shown + ms(5000)), (false, None));
        // A press ends the warm spell, so the next control waits.
        assert!(!clock.hover(MENU, shown + ms(100)));
        assert_eq!(clock.shown(shown + ms(100)), None);
        assert_eq!(clock.shown(shown + ms(600)), MENU);
        // Back on the pressed control after leaving it: shown again.
        clock.hover(None, shown + ms(700));
        clock.hover(PLUS, shown + ms(3000));
        assert_eq!(clock.shown(shown + ms(3500)), PLUS);
    }

    #[test]
    fn a_press_while_waiting_cancels_the_pending_tooltip() {
        let t0 = Instant::now();
        let mut clock = TooltipClock::default();
        clock.hover(PLUS, t0);
        assert!(!clock.press(t0 + ms(200)));
        assert_eq!(clock.shown(t0 + ms(900)), None);
        assert_eq!(clock.poll(t0 + ms(900)), (false, None));
    }
}

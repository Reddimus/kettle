//! Same-connection Wayland gestures and destination activation.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const GESTURE_AGE: Duration = Duration::from_secs(5);
const MAX_SEATS: usize = 16;

#[derive(Default)]
pub(crate) struct DragOutcome {
    performed: bool,
    cancelled: bool,
}

impl DragOutcome {
    pub(crate) fn release(&mut self) {
        self.performed = true;
    }

    pub(crate) fn cancel(&mut self) {
        self.cancelled = true;
    }

    pub(crate) fn succeeded(&self) -> bool {
        self.performed && !self.cancelled
    }
}

#[derive(Clone, Copy)]
struct Gesture {
    window: u64,
    serial: u32,
    at: Instant,
}

#[derive(Default)]
struct Gestures(BTreeMap<u32, Gesture>);

#[derive(Default)]
struct PrimaryPresses(BTreeMap<u32, (u64, u32)>);

impl PrimaryPresses {
    fn button(&mut self, seat: u32, window: Option<u64>, button: u32, pressed: bool, serial: u32) {
        if button != 0x110 {
            return;
        }
        self.0.remove(&seat);
        if pressed
            && let Some(window) = window
            && self.0.len() < MAX_SEATS
        {
            self.0.insert(seat, (window, serial));
        }
    }

    fn take(&mut self, seat: u32, window: u64) -> Option<u32> {
        if self.0.get(&seat)?.0 != window {
            return None;
        }
        self.0.remove(&seat).map(|(_, serial)| serial)
    }
}

impl Gestures {
    fn press(&mut self, seat: u32, window: u64, serial: u32, at: Instant) {
        if self.0.contains_key(&seat) || self.0.len() < MAX_SEATS {
            self.0.insert(seat, Gesture { window, serial, at });
        }
    }

    fn take(&mut self, window: u64, now: Instant) -> Option<(u32, u32)> {
        self.0
            .retain(|_, gesture| now.saturating_duration_since(gesture.at) < GESTURE_AGE);
        let seat = self
            .0
            .iter()
            .filter(|(_, gesture)| gesture.window == window)
            .max_by_key(|(_, gesture)| gesture.at)
            .map(|(&seat, _)| seat)?;
        self.0.remove(&seat).map(|gesture| (seat, gesture.serial))
    }
}

#[cfg(target_os = "linux")]
mod native;
#[cfg(target_os = "linux")]
pub(crate) use native::{Bridge, FileUpdate, Point, Update};

#[cfg(any(target_os = "linux", all(test, unix)))]
mod uri_list;
#[cfg(any(target_os = "linux", all(test, unix)))]
mod uri_reader;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_drag_uses_only_its_held_primary_press_on_that_seat_and_window() {
        let mut presses = PrimaryPresses::default();
        presses.button(1, Some(7), 0x111, true, 40);
        assert_eq!(presses.take(1, 7), None, "secondary button");
        presses.button(1, Some(7), 0x110, true, 41);
        assert_eq!(presses.take(2, 7), None, "another seat");
        assert_eq!(presses.take(1, 8), None, "another window");
        assert_eq!(presses.take(1, 7), Some(41));
        assert_eq!(presses.take(1, 7), None, "press consumed once");
    }

    #[test]
    fn release_and_unknown_origin_end_the_grab_but_serial_wrap_does_not() {
        let mut presses = PrimaryPresses::default();
        presses.button(1, Some(7), 0x110, true, u32::MAX);
        presses.button(1, None, 0x110, false, 0);
        assert_eq!(presses.take(1, 7), None, "released grab");
        presses.button(1, Some(7), 0x110, true, 0);
        assert_eq!(presses.take(1, 7), Some(0));
        presses.button(1, Some(7), 0x110, true, 1);
        presses.button(1, None, 0x110, true, 2);
        assert_eq!(presses.take(1, 7), None, "new press on an unknown surface");
    }

    #[test]
    fn activation_uses_a_recent_press_from_its_source_once() {
        let now = Instant::now();
        let mut inputs = Gestures::default();
        inputs.press(1, 7, 41, now);
        inputs.press(2, 8, 42, now);
        assert_eq!(inputs.take(9, now), None, "another window's input");
        assert_eq!(inputs.take(7, now), Some((1, 41)));
        assert_eq!(inputs.take(7, now), None, "consumed input reused");
        assert_eq!(inputs.take(8, now + GESTURE_AGE), None, "expired input");
    }

    #[test]
    fn a_later_press_replaces_that_seats_old_window() {
        let now = Instant::now();
        let mut inputs = Gestures::default();
        inputs.press(1, 7, u32::MAX, now);
        inputs.press(1, 8, 0, now);
        assert_eq!(inputs.take(7, now), None);
        assert_eq!(inputs.take(8, now), Some((1, 0)), "serials may wrap");
    }
}

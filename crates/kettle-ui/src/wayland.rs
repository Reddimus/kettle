//! Same-connection Wayland gestures and destination activation.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

const GESTURE_AGE: Duration = Duration::from_secs(5);
const MAX_SEATS: usize = 16;

#[derive(Clone, Copy)]
struct Gesture {
    window: u64,
    serial: u32,
    at: Instant,
}

#[derive(Default)]
struct Gestures(BTreeMap<u32, Gesture>);

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
pub(crate) use native::Bridge;

#[cfg(test)]
mod tests {
    use super::*;

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

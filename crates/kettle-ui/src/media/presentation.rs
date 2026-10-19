//! Placement of a manual preview without changing its media state.

use crate::mux::Rect;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Overlay {
    pub pane: u64,
    pub origin: Option<u64>,
    pub control: Option<u64>,
    pub theater: bool,
    /// Only fullscreen entered by this viewer is restored when it closes.
    pub fullscreen: bool,
}

impl Overlay {
    pub fn toggle_fullscreen(&mut self, current: bool) -> bool {
        let next = !current;
        self.fullscreen = next;
        next
    }

    pub fn step_control(&mut self, shown: &[u64], forward: bool) {
        let index = self
            .control
            .and_then(|part| shown.iter().position(|at| *at == part));
        self.control = match (index, forward) {
            (Some(index), true) => shown.get(index + 1).or_else(|| shown.first()),
            (Some(index), false) => index
                .checked_sub(1)
                .and_then(|at| shown.get(at))
                .or_else(|| shown.last()),
            (None, true) => shown.first(),
            (None, false) => shown.last(),
        }
        .copied();
    }
}

/// Center within the pane, leaving five percent padding on every side.
pub(crate) fn centered_rect(pane: Rect) -> Option<Rect> {
    let (x, y, width, height) = pane;
    if ![x, y, width, height].into_iter().all(f32::is_finite) || width <= 0.0 || height <= 0.0 {
        return None;
    }
    let right = x + width;
    let bottom = y + height;
    let rect = (
        x + width * 0.05,
        y + height * 0.05,
        width * 0.9,
        height * 0.9,
    );
    (right.is_finite()
        && bottom.is_finite()
        && rect.0 > x
        && rect.1 > y
        && rect.2 > 0.0
        && rect.3 > 0.0
        && rect.0 + rect.2 < right
        && rect.1 + rect.3 < bottom)
        .then_some(rect)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_owns_only_fullscreen_it_entered() {
        let mut overlay = Overlay {
            pane: 1,
            origin: None,
            control: None,
            theater: true,
            fullscreen: false,
        };
        assert!(overlay.toggle_fullscreen(false));
        assert!(overlay.fullscreen);
        assert!(!overlay.toggle_fullscreen(true));
        assert!(!overlay.fullscreen);
        assert!(overlay.theater);
        assert!(!overlay.toggle_fullscreen(true));
        assert!(!overlay.fullscreen);
        assert!(overlay.toggle_fullscreen(false));
        // The compositor may already have exited fullscreen independently.
        assert!(overlay.toggle_fullscreen(false));
        assert!(overlay.fullscreen);
    }

    #[test]
    fn keyboard_focus_wraps_only_through_shown_controls() {
        let mut overlay = Overlay {
            theater: false,
            fullscreen: false,
            pane: 9,
            origin: Some(3),
            control: None,
        };
        for expected in [3, 4, 5, 3] {
            overlay.step_control(&[3, 4, 5], true);
            assert_eq!(overlay.control, Some(expected));
        }
        overlay.step_control(&[3, 4, 5], false);
        assert_eq!(overlay.control, Some(5));
        overlay.step_control(&[], true);
        assert_eq!(overlay.control, None);
        assert_eq!((overlay.pane, overlay.origin), (9, Some(3)));
    }

    #[test]
    fn disappearing_controls_do_not_keep_keyboard_focus() {
        let mut overlay = Overlay {
            theater: false,
            fullscreen: false,
            pane: 9,
            origin: None,
            control: Some(4),
        };
        overlay.step_control(&[3, 5], true);
        assert_eq!(overlay.control, Some(3));
        overlay.control = Some(4);
        overlay.step_control(&[3, 5], false);
        assert_eq!(overlay.control, Some(5));
    }

    #[test]
    fn viewer_is_centered_in_its_own_split() {
        assert_eq!(
            centered_rect((400.0, 80.0, 600.0, 300.0)),
            Some((430.0, 95.0, 540.0, 270.0))
        );
    }

    #[test]
    fn resized_and_narrow_splits_keep_padding_on_both_axes() {
        for (width, height) in [(1.0, 1.0), (17.0, 480.0), (1280.0, 720.0)] {
            let (x, y, w, h) = centered_rect((20.0, 30.0, width, height)).unwrap();
            assert!(x > 20.0 && y > 30.0);
            assert!(x + w < 20.0 + width && y + h < 30.0 + height);
            assert!((w / width - 0.9).abs() < 0.0001);
            assert!((h / height - 0.9).abs() < 0.0001);
        }
    }

    #[test]
    fn invalid_or_unrepresentable_placement_is_not_drawn() {
        for pane in [
            (0.0, 0.0, 0.0, 100.0),
            (0.0, 0.0, 100.0, -1.0),
            (f32::NAN, 0.0, 100.0, 100.0),
            (0.0, 0.0, f32::INFINITY, 100.0),
            (f32::MAX, 0.0, f32::MAX, 100.0),
            (1e30, 0.0, 1.0, 100.0),
        ] {
            assert!(centered_rect(pane).is_none(), "{pane:?}");
        }
    }
}

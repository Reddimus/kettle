//! In-process tab tear-off state machine. Live PTYs move between windows
//! without respawning.
//!
//! ```text
//!   Idle  ──MouseDown on tab──▶  ArmedInside { tab_idx, started_at }
//!     ▲                            │
//!     │                            ├──MouseMove (small)──▶ stays Armed
//!     │                            ├──MouseMove (large)──▶ DraggingInside
//!     │                            └──MouseUp──▶ click (not drag) → Idle
//!     │
//!     │   DraggingInside { tab_idx }
//!     │      │
//!     │      ├──cursor leaves the window──▶  DraggingOutside
//!     │      ├──cursor ≥ 1.5×bar_h from the tab band──▶ TEAR
//!     │      └──MouseUp inside──▶  reorder within window → Idle
//!     │
//!     │   DraggingOutside { tab_idx }
//!     │      ├──cursor re-enters──▶  DraggingInside
//!     │      ├──(non-Wayland) the band threshold already tore — release
//!     │      │     here only happens within the hysteresis slop → Idle
//!     │      └──MouseUp──▶  WAYLAND ONLY: tear off at the drop point
//!     │                     (no client-side positioning mid-drag there)
//!     │
//!     └─────────── (Escape / focus loss → cancel) ─────────┘
//! ```
//!
//! Outside detection uses cursor position because Windows can keep delivering
//! moves while suppressing leave events during capture.

/// Drag state for detachable tabs.
#[derive(Debug, Clone, Default)]
pub enum DragState {
    /// No drag in progress.
    #[default]
    Idle,
    /// Mouse-down landed on the tab bar; not yet a drag. The
    /// click-vs-drag distinguisher is pure distance from the press origin
    /// (`DRAG_DISTANCE_THRESHOLD_PX`), matching GTK / OS drag thresholds.
    ArmedInside { tab_idx: usize },
    /// Mouse moved enough after ArmedInside to qualify as a drag. The
    /// cursor is still inside this kettle window. Except on Wayland,
    /// crossing the band threshold from EITHER dragging state tears
    /// immediately (`maybe_tear_off` consumes the FSM mid-drag).
    DraggingInside { tab_idx: usize },
    /// Cursor left this kettle window during a drag. On Wayland a
    /// mouse-up here is the tear-off (the at-release fallback); on every
    /// other platform the band threshold already tore before this could
    /// matter beyond the hysteresis slop.
    DraggingOutside { tab_idx: usize },
}

impl DragState {
    /// Transition Idle → ArmedInside on mouse-down
    /// over a tab. Returns the new state; caller stores it.
    pub fn on_mouse_down_on_tab(tab_idx: usize) -> Self {
        DragState::ArmedInside { tab_idx }
    }

    /// Threshold below which a MouseMove from ArmedInside stays
    /// Armed (a hand-twitch on a precise mouse). Above which the
    /// state transitions to DraggingInside. 4px matches GTK +
    /// the OS-native drag-distance most desktops ship.
    pub const DRAG_DISTANCE_THRESHOLD_PX: f32 = 4.0;

    /// ArmedInside → DraggingInside transition on
    /// mouse-move with distance > threshold. Sub-pixel moves
    /// stay Armed (real click intent). Returns the new state
    /// or self unchanged if not a drag.
    pub fn on_mouse_move(self, dx: f32, dy: f32) -> Self {
        match self {
            DragState::ArmedInside { tab_idx }
                if (dx * dx + dy * dy).sqrt() > Self::DRAG_DISTANCE_THRESHOLD_PX =>
            {
                DragState::DraggingInside { tab_idx }
            }
            other => other,
        }
    }

    /// Any-state → Idle on mouse-up.
    /// Caller is responsible for any actual drop logic (which
    /// tab to move where) before calling this; this just
    /// resets the FSM.
    pub fn on_mouse_up(self) -> Self {
        DragState::Idle
    }

    /// Cancel path (phase 9 of docs/TERMINATOR-DETACHABLE-TABS-DESIGN.md).
    /// Returns Some(tab_idx) if a tab was being dragged when the cancel
    /// fired, so the caller can restore that tab's visual state (clear
    /// ghost, reset focus). None when the cancel comes from Idle (no-op).
    pub fn cancel(self) -> (Self, Option<usize>) {
        match self {
            DragState::Idle => (DragState::Idle, None),
            DragState::ArmedInside { tab_idx, .. } => (DragState::Idle, Some(tab_idx)),
            DragState::DraggingInside { tab_idx, .. } => (DragState::Idle, Some(tab_idx)),
            DragState::DraggingOutside { tab_idx, .. } => (DragState::Idle, Some(tab_idx)),
        }
    }

    /// Transition DraggingInside → DraggingOutside when the
    /// cursor leaves the window (event-driven OR position-derived).
    /// Returns self unchanged from non-DraggingInside.
    pub fn on_cursor_leave_window(self) -> Self {
        match self {
            DragState::DraggingInside { tab_idx, .. } => DragState::DraggingOutside { tab_idx },
            other => other,
        }
    }

    /// Transition DraggingOutside → DraggingInside on
    /// cursor-re-entered-this-window event (user changed their
    /// mind mid-drag). Preserves the original tab_idx. Returns
    /// self unchanged from non-DraggingOutside.
    pub fn on_cursor_reenter_window(self) -> Self {
        match self {
            DragState::DraggingOutside { tab_idx } => DragState::DraggingInside { tab_idx },
            other => other,
        }
    }
}

/// All inputs share one desktop coordinate system.
pub(crate) fn frame_grab_offset(
    client_origin: (f64, f64),
    frame_origin: (f64, f64),
    press: (f64, f64),
) -> (f64, f64) {
    (
        client_origin.0 - frame_origin.0 + press.0,
        client_origin.1 - frame_origin.1 + press.1,
    )
}

pub(crate) struct ManualDragStep {
    pub(crate) cursor: (f64, f64),
    pub(crate) frame_origin: (f64, f64),
}

pub(crate) fn manual_drag_step(
    client_origin: (f64, f64),
    pointer: (f64, f64),
    grab: (f64, f64),
) -> ManualDragStep {
    let cursor = (client_origin.0 + pointer.0, client_origin.1 + pointer.1);
    ManualDragStep {
        cursor,
        frame_origin: (cursor.0 - grab.0, cursor.1 - grab.1),
    }
}

pub(crate) fn client_to_desktop(origin: (f64, f64), pointer: (f64, f64), scale: f64) -> (f64, f64) {
    (origin.0 + pointer.0 / scale, origin.1 + pointer.1 / scale)
}

pub(crate) fn desktop_to_client(origin: (f64, f64), pointer: (f64, f64), scale: f64) -> (f64, f64) {
    (
        (pointer.0 - origin.0) * scale,
        (pointer.1 - origin.1) * scale,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mixed_scale_docking_uses_the_target_windows_scale() {
        let screen = client_to_desktop((500.0, 100.0), (120.0, 24.0), 2.0);
        assert_eq!(screen, (560.0, 112.0));
        assert_eq!(
            desktop_to_client((500.0, 100.0), screen, 2.0),
            (120.0, 24.0)
        );
        assert_eq!(desktop_to_client((540.0, 100.0), screen, 1.0), (20.0, 12.0));
        assert_eq!(desktop_to_client((540.0, 100.0), screen, 1.5), (30.0, 18.0));
    }

    #[test]
    fn negative_monitor_origins_and_physical_desktops_keep_their_coordinates() {
        let screen = client_to_desktop((-900.0, -200.0), (40.0, 20.0), 1.0);
        assert_eq!(screen, (-860.0, -180.0));
        assert_eq!(
            desktop_to_client((-900.0, -200.0), screen, 1.0),
            (40.0, 20.0)
        );
    }

    #[test]
    fn lone_drag_first_motion_keeps_press_anchor_and_hits_target() {
        let frame = (385.0, 266.0);
        let client = (385.0, 298.0);
        let grab = frame_grab_offset(client, frame, (130.0, 12.0));
        // First motion has crossed all the way to the sibling's tab band.
        let step = manual_drag_step(client, (-200.0, -174.0), grab);
        assert_eq!(step.cursor, (185.0, 124.0));
        assert_eq!(step.frame_origin, (55.0, 80.0));
        // After following, the pointer is still at the original press.
        let next = manual_drag_step((55.0, 112.0), (130.0, 12.0), grab);
        assert_eq!(next.cursor, step.cursor);
        assert_eq!(next.frame_origin, step.frame_origin);
    }

    #[test]
    fn drag_coordinates_preserve_negative_and_scaled_physical_positions() {
        let frame = (-1800.0, -500.0);
        let client = (-1800.0, -436.0);
        let grab = frame_grab_offset(client, frame, (260.0, 24.0));
        let step = manual_drag_step(client, (400.0, -150.0), grab);
        assert_eq!(step.cursor, (-1400.0, -586.0));
        assert_eq!(step.frame_origin, (-1660.0, -674.0));
    }

    #[test]
    fn idle_to_armed_on_mouse_down() {
        let s = DragState::on_mouse_down_on_tab(3);
        assert!(matches!(s, DragState::ArmedInside { tab_idx: 3 }));
    }

    #[test]
    fn armed_to_dragging_only_above_threshold() {
        let s = DragState::on_mouse_down_on_tab(0);
        // Tiny move stays Armed (a hand-twitch is a click, not a drag).
        let s2 = s.clone().on_mouse_move(1.0, 1.0);
        assert!(matches!(s2, DragState::ArmedInside { .. }));
        // Large move transitions to DraggingInside.
        let s3 = s.on_mouse_move(10.0, 10.0);
        assert!(matches!(s3, DragState::DraggingInside { .. }));
    }

    #[test]
    fn any_state_to_idle_on_mouse_up() {
        for s in &[
            DragState::Idle,
            DragState::ArmedInside { tab_idx: 0 },
            DragState::DraggingInside { tab_idx: 0 },
            DragState::DraggingOutside { tab_idx: 0 },
        ] {
            assert!(matches!(s.clone().on_mouse_up(), DragState::Idle));
        }
    }

    #[test]
    fn cancel_returns_dragged_tab_idx() {
        // Drift guard: cancel() reports the tab that was being
        // manipulated so the caller can restore its visual state.
        let (s, restored) = DragState::Idle.cancel();
        assert!(matches!(s, DragState::Idle));
        assert!(restored.is_none());
        let (s, restored) = DragState::ArmedInside { tab_idx: 5 }.cancel();
        assert!(matches!(s, DragState::Idle));
        assert_eq!(restored, Some(5));
        let (s, restored) = DragState::DraggingInside { tab_idx: 7 }.cancel();
        assert!(matches!(s, DragState::Idle));
        assert_eq!(restored, Some(7));
        let (s, restored) = DragState::DraggingOutside { tab_idx: 9 }.cancel();
        assert!(matches!(s, DragState::Idle));
        assert_eq!(restored, Some(9));
    }

    #[test]
    fn cursor_leave_and_reenter_window_transitions() {
        // DraggingInside ↔ DraggingOutside on leave / re-enter; other
        // states pass through unchanged.
        let s = DragState::DraggingInside { tab_idx: 3 }.on_cursor_leave_window();
        assert!(matches!(s, DragState::DraggingOutside { tab_idx: 3 }));
        let s = s.on_cursor_reenter_window();
        assert!(matches!(s, DragState::DraggingInside { tab_idx: 3 }));
        assert!(matches!(
            DragState::Idle.on_cursor_leave_window(),
            DragState::Idle
        ));
        assert!(matches!(
            DragState::ArmedInside { tab_idx: 0 }.on_cursor_reenter_window(),
            DragState::ArmedInside { .. }
        ));
    }

    #[test]
    fn end_to_end_drag_walkthrough() {
        // Pure-FSM e2e drift guard: the full tear-off gesture flow.
        // Idle → ArmedInside → DraggingInside → DraggingOutside, then a
        // cancel restores. The caller tears at the band THRESHOLD mid-drag
        // (`maybe_tear_off` resets the FSM to Idle); a mouse-up while
        // outside is the Wayland-only at-release tear (see the Released arm
        // in app.rs).
        let s = DragState::on_mouse_down_on_tab(2);
        assert!(matches!(s, DragState::ArmedInside { .. }));
        let s = s.on_mouse_move(1.0, 1.0);
        assert!(matches!(s, DragState::ArmedInside { .. }));
        let s = s.on_mouse_move(20.0, 10.0);
        assert!(matches!(s, DragState::DraggingInside { tab_idx: 2 }));
        let s = s.on_cursor_leave_window();
        assert!(matches!(s, DragState::DraggingOutside { tab_idx: 2 }));
        let (s, restored) = s.cancel();
        assert!(matches!(s, DragState::Idle));
        assert_eq!(restored, Some(2));
    }
}

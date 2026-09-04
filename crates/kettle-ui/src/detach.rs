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
    /// cursor is still inside this kettle window. v2.19.0: crossing the
    /// band threshold from EITHER dragging state tears immediately
    /// (`maybe_tear_off` consumes the FSM mid-drag).
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

    /// Terminator parity, detachable-tabs Bucket-D, phase 9 of
    /// docs/TERMINATOR-DETACHABLE-TABS-DESIGN.md: cancel path. Returns
    /// Some(tab_idx) if a tab was being dragged when the cancel fired —
    /// caller can restore that tab's visual state (clear ghost, reset
    /// focus). None when the cancel comes from Idle (no-op).
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

#[cfg(test)]
mod tests {
    use super::*;

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
        // Pure-FSM e2e drift guard: the full C6 tear-off gesture flow.
        // Idle → ArmedInside → DraggingInside → DraggingOutside, then a
        // cancel restores. v2.19.0: the caller tears at the band
        // THRESHOLD mid-drag (`maybe_tear_off` resets the FSM to Idle);
        // a mouse-up while outside is the Wayland-only at-release tear
        // (see the Released arm in app.rs).
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

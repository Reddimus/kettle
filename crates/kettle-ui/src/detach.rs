//! Cycle 400 (Terminator parity, detachable-tabs Bucket-D
//! sub-cycle 5): cross-window tab-drag state machine. Lives in
//! its own module so the App's mouse-handler stays small + the
//! states are unit-testable as a pure FSM.
//!
//! Per docs/TERMINATOR-DETACHABLE-TABS-DESIGN.md sub-cycle 5:
//!
//! ```text
//!   Idle  ──MouseDown on tab──▶  ArmedInside { tab_idx, started_at }
//!     ▲                            │
//!     │                            ├──MouseMove (small)──▶ stays Armed
//!     │                            ├──MouseMove (large)──▶ DraggingInside
//!     │                            └──MouseUp──▶ click (not drag) → Idle
//!     │
//!     │   DraggingInside { tab_idx, ghost_x, ghost_y }
//!     │      │
//!     │      ├──CursorLeft──▶  DraggingOutside { ipc_session }
//!     │      └──MouseUp inside──▶  reorder within window → Idle
//!     │
//!     │   DraggingOutside { ipc_session }
//!     │      │
//!     │      ├──CursorEntersOtherKettle──▶  PendingDrop { target_window }
//!     │      └──MouseUp over empty space──▶ NewWindowOnDrop
//!     │
//!     └─────────── (Escape, IPC failure, abort) ─────────┘
//! ```
//!
//! Sub-cycles 6 (cursor detection), 7 (IPC handshake + fd
//! transfer), 8 (new-window-on-drop), 9 (cancel path), 11 (e2e
//! test) wire each transition. This cycle ships the enum
//! shape + minimal in-process transitions (Idle ↔ ArmedInside ↔
//! DraggingInside) as the foundation those sub-cycles compose.

#![allow(dead_code)]

use std::time::Instant;

/// Drag-state machine for detachable tabs. Each variant carries
/// just the data its transitions need; the App's mouse-handler
/// uses the type-state pattern to ensure only legal transitions
/// happen (no "drag from no-armed state" footgun).
#[derive(Debug, Clone, Default)]
pub enum DragState {
    /// No drag in progress.
    #[default]
    Idle,
    /// Mouse-down landed on the tab bar; not yet a drag.
    /// `started_at` is used by the click-vs-drag distinguisher
    /// (a quick MouseDown→MouseUp is a click, not a drag).
    ArmedInside { tab_idx: usize, started_at: Instant },
    /// Mouse moved enough after ArmedInside to qualify as a
    /// drag. The cursor is still inside this kettle window.
    DraggingInside {
        tab_idx: usize,
        ghost_x: f32,
        ghost_y: f32,
    },
    /// Cursor left this kettle window during a drag. The drag
    /// state machine hands off to the cross-window IPC layer
    /// (sub-cycle 6+7) which advances to PendingDrop on cursor-
    /// over-other-kettle-window.
    DraggingOutside {
        tab_idx: usize,
        /// IPC session token (sub-cycle 7 fills in).
        session_id: u64,
    },
}

impl DragState {
    /// Cycle 400: transition Idle → ArmedInside on mouse-down
    /// over a tab. Returns the new state; caller stores it.
    pub fn on_mouse_down_on_tab(tab_idx: usize) -> Self {
        DragState::ArmedInside {
            tab_idx,
            started_at: Instant::now(),
        }
    }

    /// Threshold below which a MouseMove from ArmedInside stays
    /// Armed (a hand-twitch on a precise mouse). Above which the
    /// state transitions to DraggingInside. 4px matches GTK +
    /// the OS-native drag-distance most desktops ship.
    pub const DRAG_DISTANCE_THRESHOLD_PX: f32 = 4.0;

    /// Cycle 400: ArmedInside → DraggingInside transition on
    /// mouse-move with distance > threshold. Sub-pixel moves
    /// stay Armed (real click intent). Returns the new state
    /// or self unchanged if not a drag.
    pub fn on_mouse_move(self, dx: f32, dy: f32) -> Self {
        match self {
            DragState::ArmedInside {
                tab_idx,
                started_at: _,
            } if (dx * dx + dy * dy).sqrt() > Self::DRAG_DISTANCE_THRESHOLD_PX => {
                DragState::DraggingInside {
                    tab_idx,
                    ghost_x: dx,
                    ghost_y: dy,
                }
            }
            DragState::DraggingInside { tab_idx, .. } => DragState::DraggingInside {
                tab_idx,
                ghost_x: dx,
                ghost_y: dy,
            },
            other => other,
        }
    }

    /// Cycle 400: any-state → Idle on mouse-up.
    /// Caller is responsible for any actual drop logic (which
    /// tab to move where) before calling this; this just
    /// resets the FSM.
    pub fn on_mouse_up(self) -> Self {
        DragState::Idle
    }

    /// Cycle 400: any-state → Idle on Escape / abort.
    pub fn on_abort(self) -> Self {
        DragState::Idle
    }

    /// True when the user is actively dragging (vs Idle or just-
    /// armed). Used by the renderer to draw the drag ghost.
    pub fn is_dragging(&self) -> bool {
        matches!(
            self,
            DragState::DraggingInside { .. } | DragState::DraggingOutside { .. }
        )
    }

    /// Cycle 401 (Terminator parity, detachable-tabs Bucket-D
    /// sub-cycle 9): cancel path. Returns Some(tab_idx) if a tab
    /// was being dragged when the cancel fired — caller can
    /// restore that tab's visual state (clear ghost, reset focus).
    /// None when the cancel comes from Idle (no-op).
    pub fn cancel(self) -> (Self, Option<usize>) {
        match self {
            DragState::Idle => (DragState::Idle, None),
            DragState::ArmedInside { tab_idx, .. } => (DragState::Idle, Some(tab_idx)),
            DragState::DraggingInside { tab_idx, .. } => (DragState::Idle, Some(tab_idx)),
            DragState::DraggingOutside { tab_idx, .. } => (DragState::Idle, Some(tab_idx)),
        }
    }

    /// Cycle 401: transition DraggingInside → DraggingOutside on
    /// cursor-leaves-window event. Captures a fresh session_id
    /// for the cross-process IPC handshake (sub-cycle 7 fills
    /// in). Returns self unchanged from non-DraggingInside.
    pub fn on_cursor_leave_window(self, session_id: u64) -> Self {
        match self {
            DragState::DraggingInside { tab_idx, .. } => DragState::DraggingOutside {
                tab_idx,
                session_id,
            },
            other => other,
        }
    }

    /// Cycle 401: transition DraggingOutside → DraggingInside on
    /// cursor-re-entered-this-window event (user changed their
    /// mind mid-drag). Preserves the original tab_idx. Returns
    /// self unchanged from non-DraggingOutside.
    pub fn on_cursor_reenter_window(self, ghost_x: f32, ghost_y: f32) -> Self {
        match self {
            DragState::DraggingOutside { tab_idx, .. } => DragState::DraggingInside {
                tab_idx,
                ghost_x,
                ghost_y,
            },
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
        match s {
            DragState::ArmedInside { tab_idx, .. } => assert_eq!(tab_idx, 3),
            _ => panic!("expected ArmedInside"),
        }
    }

    #[test]
    fn armed_to_dragging_only_above_threshold() {
        let s = DragState::on_mouse_down_on_tab(0);
        // Tiny move stays Armed.
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
            DragState::ArmedInside {
                tab_idx: 0,
                started_at: Instant::now(),
            },
            DragState::DraggingInside {
                tab_idx: 0,
                ghost_x: 0.0,
                ghost_y: 0.0,
            },
            DragState::DraggingOutside {
                tab_idx: 0,
                session_id: 0,
            },
        ] {
            assert!(matches!(s.clone().on_mouse_up(), DragState::Idle));
        }
    }

    #[test]
    fn any_state_to_idle_on_abort() {
        let s = DragState::DraggingInside {
            tab_idx: 7,
            ghost_x: 100.0,
            ghost_y: 200.0,
        };
        assert!(matches!(s.on_abort(), DragState::Idle));
    }

    #[test]
    fn cancel_returns_dragged_tab_idx() {
        // Cycle 401 drift guard. cancel() reports the tab that
        // was being manipulated so the caller can restore its
        // visual state.
        let (s, restored) = DragState::Idle.cancel();
        assert!(matches!(s, DragState::Idle));
        assert!(restored.is_none());
        let (s, restored) = DragState::ArmedInside {
            tab_idx: 5,
            started_at: Instant::now(),
        }
        .cancel();
        assert!(matches!(s, DragState::Idle));
        assert_eq!(restored, Some(5));
        let (s, restored) = DragState::DraggingInside {
            tab_idx: 7,
            ghost_x: 0.0,
            ghost_y: 0.0,
        }
        .cancel();
        assert!(matches!(s, DragState::Idle));
        assert_eq!(restored, Some(7));
        let (s, restored) = DragState::DraggingOutside {
            tab_idx: 9,
            session_id: 0,
        }
        .cancel();
        assert!(matches!(s, DragState::Idle));
        assert_eq!(restored, Some(9));
    }

    #[test]
    fn cursor_leave_and_reenter_window_transitions() {
        // Cycle 401: DraggingInside ↔ DraggingOutside transitions
        // on cursor-leave / cursor-reenter events.
        let s = DragState::DraggingInside {
            tab_idx: 3,
            ghost_x: 100.0,
            ghost_y: 50.0,
        };
        let s = s.on_cursor_leave_window(42);
        match s {
            DragState::DraggingOutside {
                tab_idx,
                session_id,
            } => {
                assert_eq!(tab_idx, 3);
                assert_eq!(session_id, 42);
            }
            _ => panic!("expected DraggingOutside"),
        }
        let s = DragState::DraggingOutside {
            tab_idx: 3,
            session_id: 42,
        };
        let s = s.on_cursor_reenter_window(200.0, 100.0);
        match s {
            DragState::DraggingInside {
                tab_idx,
                ghost_x,
                ghost_y,
            } => {
                assert_eq!(tab_idx, 3);
                assert_eq!(ghost_x, 200.0);
                assert_eq!(ghost_y, 100.0);
            }
            _ => panic!("expected DraggingInside"),
        }
        // Non-Dragging states unchanged.
        let s = DragState::Idle.on_cursor_leave_window(99);
        assert!(matches!(s, DragState::Idle));
        let s = DragState::ArmedInside {
            tab_idx: 0,
            started_at: Instant::now(),
        }
        .on_cursor_reenter_window(0.0, 0.0);
        assert!(matches!(s, DragState::ArmedInside { .. }));
    }

    #[test]
    fn end_to_end_drag_walkthrough() {
        // Cycle 401 (Terminator parity, detachable-tabs Bucket-D
        // sub-cycle 11 partial): pure-FSM e2e drift guard.
        // Walks the full drag flow: Idle → ArmedInside →
        // DraggingInside → DraggingOutside → cancel → Idle.
        let s = DragState::Idle;
        assert!(!s.is_dragging());
        // Mouse down on tab 2.
        let s = DragState::on_mouse_down_on_tab(2);
        assert!(matches!(s, DragState::ArmedInside { .. }));
        // Tiny move stays Armed.
        let s = s.on_mouse_move(1.0, 1.0);
        assert!(matches!(s, DragState::ArmedInside { .. }));
        // Larger move starts dragging.
        let s = s.on_mouse_move(20.0, 10.0);
        assert!(matches!(s, DragState::DraggingInside { tab_idx: 2, .. }));
        assert!(s.is_dragging());
        // Cursor leaves window.
        let s = s.on_cursor_leave_window(100);
        assert!(matches!(
            s,
            DragState::DraggingOutside {
                tab_idx: 2,
                session_id: 100,
            }
        ));
        assert!(s.is_dragging());
        // User cancels with Escape.
        let (s, restored) = s.cancel();
        assert!(matches!(s, DragState::Idle));
        assert_eq!(restored, Some(2));
        assert!(!s.is_dragging());
    }

    #[test]
    fn is_dragging_only_during_active_drag() {
        assert!(!DragState::Idle.is_dragging());
        assert!(
            !DragState::ArmedInside {
                tab_idx: 0,
                started_at: Instant::now(),
            }
            .is_dragging()
        );
        assert!(
            DragState::DraggingInside {
                tab_idx: 0,
                ghost_x: 0.0,
                ghost_y: 0.0,
            }
            .is_dragging()
        );
        assert!(
            DragState::DraggingOutside {
                tab_idx: 0,
                session_id: 0,
            }
            .is_dragging()
        );
    }
}

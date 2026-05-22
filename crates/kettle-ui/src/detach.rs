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
#[derive(Debug, Clone)]
pub enum DragState {
    /// No drag in progress.
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

impl Default for DragState {
    fn default() -> Self {
        DragState::Idle
    }
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

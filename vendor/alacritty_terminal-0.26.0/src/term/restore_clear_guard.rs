//! Kettle-only: keep rows seeded by a session restore on screen when the new
//! shell clears at startup.
//!
//! Kettle parses restored rows and a divider into a fresh terminal before the
//! shell starts. A startup `clear` would otherwise push them into history (ED
//! 2) and leave a bare prompt. While armed, the first ED 2 issued from the home
//! position erases only from the first row below the divider, so the restored
//! rows stay in view and the prompt lands under them.
//!
//! The guard is one-shot and unarmed by default; every hook below is a single
//! `Option` test while it is unarmed, so terminals that never arm it behave as
//! upstream. ED 3 never touches it. Anything that suggests the shell or the
//! user has moved on disarms it: another erase, a scroll, the alternate
//! screen, a reset, user input, or a resize after output arrived.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use log::debug;

use crate::grid::Dimensions;
use crate::index::{Column, Line, Point};
use crate::term::{Term, TermMode};

#[derive(Debug)]
pub(super) struct RestoreClearGuard {
    /// Stable identity of the first row below the restored rows: history
    /// origin plus history size plus screen line. Scrolling, eviction and ED
    /// 3 all keep a row's identity, so it is only wrong after a reflow.
    anchor: u64,
    /// Geometry the anchor was taken in. A reflow that bypasses
    /// [`Term::resize`] fails this check and the guarded clear falls back.
    columns: usize,
    screen_lines: usize,
    /// Whether program output has been parsed since arming.
    output_seen: bool,
    /// Set by the embedder's input path; read under the terminal lock.
    user_input: Arc<AtomicBool>,
}

impl RestoreClearGuard {
    fn input_arrived(&self) -> bool {
        self.user_input.load(Ordering::Acquire)
    }
}

impl<T> Term<T> {
    /// Arm the restore clear guard at the cursor row, which must be the first
    /// row below the restored rows and divider.
    ///
    /// `user_input` is shared with the embedder's input path: once it is set,
    /// the guard disarms at its next decision point. The guard is not armed
    /// on the alternate screen.
    pub fn arm_restore_clear_guard(&mut self, user_input: Arc<AtomicBool>) {
        if self.mode.contains(TermMode::ALT_SCREEN) {
            return;
        }
        let line = self.grid.cursor.point.line;
        debug!(
            "Restore clear guard armed at row {} of {}",
            line.0,
            self.screen_lines()
        );
        self.restore_clear_guard = Some(RestoreClearGuard {
            anchor: self.restore_clear_anchor(line),
            columns: self.columns(),
            screen_lines: self.screen_lines(),
            output_seen: false,
            user_input,
        });
    }

    /// Disarm the restore clear guard; a later clear behaves as upstream.
    pub fn disarm_restore_clear_guard(&mut self) {
        self.drop_restore_clear_guard("disarmed by the embedder");
    }

    /// Whether the guard is still armed and no user input has arrived.
    pub fn restore_clear_guard_armed(&self) -> bool {
        self.restore_clear_guard
            .as_ref()
            .is_some_and(|guard| !guard.input_arrived())
    }

    /// Record that program output is about to be parsed. Call this under the
    /// same lock as the parse, before it. Pending user input disarms here.
    pub fn note_restore_clear_guard_output(&mut self) {
        let Some(guard) = self.restore_clear_guard.as_mut() else {
            return;
        };
        if guard.input_arrived() {
            self.drop_restore_clear_guard("user input");
        } else {
            guard.output_seen = true;
        }
    }

    pub(super) fn drop_restore_clear_guard(&mut self, reason: &str) {
        if self.restore_clear_guard.take().is_some() {
            debug!("Restore clear guard disarmed: {reason}");
        }
    }

    /// After a resize: with no output since arming, the cursor is still where
    /// the restore left it, so take the anchor from it again. Otherwise the
    /// anchor may no longer match the rows, so disarm.
    pub(super) fn resize_restore_clear_guard(&mut self) {
        let Some(guard) = self.restore_clear_guard.as_ref() else {
            return;
        };
        if guard.input_arrived() {
            self.drop_restore_clear_guard("user input");
        } else if guard.output_seen {
            self.drop_restore_clear_guard("resized after output");
        } else if self.mode.contains(TermMode::ALT_SCREEN) {
            self.drop_restore_clear_guard("alternate screen");
        } else {
            let line = self.grid.cursor.point.line;
            let anchor = self.restore_clear_anchor(line);
            let (columns, screen_lines) = (self.columns(), self.screen_lines());
            if let Some(guard) = self.restore_clear_guard.as_mut() {
                guard.anchor = anchor;
                guard.columns = columns;
                guard.screen_lines = screen_lines;
            }
            debug!(
                "Restore clear guard re-anchored at row {} of {screen_lines}",
                line.0
            );
        }
    }

    /// Consume the guard for an ED 2 on the primary screen. Returns true when
    /// the guarded erase ran; false means the caller clears as upstream. The
    /// guard is gone afterwards either way.
    pub(super) fn consume_restore_clear_guard(&mut self) -> bool {
        let Some(guard) = self.restore_clear_guard.take() else {
            return false;
        };
        let reason = if guard.input_arrived() {
            "user input"
        } else if self.grid.cursor.point != Point::new(Line(0), Column(0)) {
            "erase away from home"
        } else if guard.columns != self.columns() || guard.screen_lines != self.screen_lines() {
            "geometry changed"
        } else {
            let row = guard
                .anchor
                .checked_sub(self.restore_clear_anchor(Line(0)))
                .filter(|&row| row > 0 && row < self.screen_lines() as u64);
            match row {
                Some(row) => {
                    let line = Line(row as i32);
                    self.grid.reset_region(line..);
                    self.grid.cursor.point = Point::new(line, Column(0));
                    self.grid.cursor.input_needs_wrap = false;
                    debug!("Restore clear guard kept {row} restored rows");
                    return true;
                }
                None => "restored rows left the screen",
            }
        };
        debug!("Restore clear guard disarmed: {reason}");
        false
    }

    fn restore_clear_anchor(&self, line: Line) -> u64 {
        self.graphics_screen_top()
            .saturating_add(line.0.max(0) as u64)
    }
}

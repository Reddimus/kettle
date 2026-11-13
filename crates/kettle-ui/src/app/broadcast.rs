//! The broadcast scope's one write path.
//!
//! A window's [`BroadcastScope`] changes only in [`App::set_broadcast_scope`],
//! which also redraws the window (its pane frames and tab accent show the
//! scope) and re-syncs the macOS menu bar, whose Broadcast Input rows carry
//! the checkmark. The broadcast actions, and the group cleanup that turns a
//! stale group scope off from the redraw, close and title paths, all go
//! through it. `the_broadcast_scope_has_one_writer` keeps it the only writer.
use super::*;

/// The scope a broadcast action leaves a window in, or `None` when the action
/// changes nothing: a group toggle while the focused pane has no group, or an
/// action that is not a broadcast action.
///
/// Every toggle turns broadcast off when its scope is already on. Under
/// `broadcast-default = off`, `ToggleBroadcastAll` has no scope to turn on,
/// so it can only turn broadcast off (CONFIG.md: `off` "prevents the chord
/// from enabling broadcast").
pub(super) fn broadcast_scope_after(
    action: &Action,
    current: &BroadcastScope,
    default: kettle_config::BroadcastDefault,
    focused_group: Option<&str>,
) -> Option<BroadcastScope> {
    let toggle = |on: BroadcastScope| {
        if *current == on {
            BroadcastScope::Off
        } else {
            on
        }
    };
    match action {
        Action::ToggleBroadcastAll => Some(toggle(broadcast_scope_for_default(default))),
        Action::ToggleBroadcastOff => Some(BroadcastScope::Off),
        Action::ToggleBroadcastGroup => {
            focused_group.map(|group| toggle(BroadcastScope::Group(group.to_owned())))
        }
        Action::ToggleBroadcastWindow => Some(toggle(BroadcastScope::All)),
        Action::ToggleBroadcastPanesInTab => Some(toggle(BroadcastScope::Tab)),
        _ => None,
    }
}

impl App {
    /// Change `ws`'s broadcast scope. This is the only place the scope is
    /// written. A scope that is already set changes nothing, so the per-frame
    /// group cleanup costs no redraw or menu rebuild.
    pub(super) fn set_broadcast_scope(&self, ws: &mut WindowState, scope: BroadcastScope) -> bool {
        if ws.mux.broadcast == scope {
            return false;
        }
        ws.mux.broadcast = scope;
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
        // The window in hand is out of `self.windows` while it dispatches,
        // so the menu must be given it rather than look it up.
        if ws.seq == self.focused_seq {
            self.sync_macos_menu(Some(ws));
        }
        true
    }

    /// [`Self::set_broadcast_scope`] for a window that is not in hand.
    pub(super) fn set_window_broadcast_scope(&mut self, seq: u64, scope: BroadcastScope) {
        if let Some(mut target) = self.windows.remove(&seq) {
            self.set_broadcast_scope(&mut target, scope);
            self.windows.insert(seq, target);
        }
    }

    /// Run a broadcast action on `ws`.
    pub(super) fn apply_broadcast_action(&self, ws: &mut WindowState, action: &Action) {
        let focused_group = ws.mux.focused().and_then(|p| p.group_name.clone());
        match broadcast_scope_after(
            action,
            &ws.mux.broadcast,
            self.cfg.broadcast_default,
            focused_group.as_deref(),
        ) {
            Some(scope) => {
                self.set_broadcast_scope(ws, scope);
            }
            None if matches!(action, Action::ToggleBroadcastGroup) => log::info!(
                "toggle-broadcast-group: focused pane has no group_name; \
                 use Action::CreateGroup or Action::GroupTab first"
            ),
            None => {}
        }
    }
}

#[cfg(test)]
#[path = "broadcast_tests.rs"]
mod tests;

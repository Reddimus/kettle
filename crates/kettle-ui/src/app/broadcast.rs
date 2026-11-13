//! The broadcast scope's one write path.
//!
//! A window's [`BroadcastScope`] changes only in [`App::set_broadcast_scope`],
//! which also redraws the window (its pane frames and tab accent show the
//! scope) and re-syncs the macOS menu bar, whose Broadcast Input rows carry
//! the checkmark. The broadcast actions, and the group cleanup that turns a
//! stale group scope off from the redraw, close and title paths, all go
//! through it. `the_broadcast_scope_has_one_writer` keeps it the only writer.
//!
//! [`broadcast_rows`] is the Broadcast Input submenu both the macOS menu bar
//! and the right-click menu show: Off, Panes in This Tab, Panes in This
//! Window and Panes in This Group, with the current scope checked.
use super::*;

/// What a window broadcasts to, seen from its focused pane. It holds no group
/// name, so the menu bar's snapshot stays `Copy`.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BroadcastKind {
    #[default]
    Off,
    Tab,
    Window,
    /// A group the focused pane belongs to.
    FocusedGroup,
    /// A group the focused pane does not belong to: it is in another group or
    /// in none.
    OtherGroup,
}

impl BroadcastKind {
    pub(crate) fn of(scope: &BroadcastScope, focused_group: Option<&str>) -> Self {
        match scope {
            BroadcastScope::Off => Self::Off,
            BroadcastScope::Tab => Self::Tab,
            BroadcastScope::All => Self::Window,
            BroadcastScope::Group(name) if focused_group == Some(name.as_str()) => {
                Self::FocusedGroup
            }
            BroadcastScope::Group(_) => Self::OtherGroup,
        }
    }

    /// `ws`'s scope, and whether its focused pane has a group.
    pub(crate) fn of_window(ws: &WindowState) -> (Self, bool) {
        // The pane the group toggle reads (`Mux::focused`).
        let group = ws
            .mux
            .active_focus()
            .and_then(|id| ws.mux.panes.get(&id))
            .and_then(|pane| pane.group_name.as_deref());
        (Self::of(&ws.mux.broadcast, group), group.is_some())
    }
}

/// One row of the Broadcast Input submenu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BroadcastRowKind {
    Off,
    Tab,
    Window,
    Group,
}

impl BroadcastRowKind {
    pub(crate) fn text(self) -> kettle_i18n::Text {
        use kettle_i18n::Text as T;
        match self {
            Self::Off => T::MenuBroadcastOff,
            Self::Tab => T::MenuBroadcastTab,
            Self::Window => T::MenuBroadcastWindow,
            Self::Group => T::MenuBroadcastGroup,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BroadcastRow {
    pub(crate) kind: BroadcastRowKind,
    pub(crate) action: Action,
    pub(crate) checked: bool,
    pub(crate) enabled: bool,
}

/// The Broadcast Input rows for a window whose scope is `kind`, under
/// `broadcast-default = default`. `pane_group` says whether the focused pane
/// has a group.
///
/// The rows look like radio buttons but run the existing toggles, so choosing
/// the checked row turns broadcast off, and choosing another switches to its
/// scope. The chord's action (`ToggleBroadcastAll`) is the action of the row
/// for the scope it turns on, so the row shows the chord: Tab under `group`,
/// Window under `all`, and no row under `off`, where the chord only stops
/// broadcasting.
///
/// The Group row is checked whenever a group is the target, so a row is always
/// checked. When the focused pane is not in that group (it is in another group
/// or in none), the group toggle would switch groups instead of stopping, so
/// the checked row runs `ToggleBroadcastOff`; choosing it again then starts
/// broadcasting to the focused pane's group. The Group row is unavailable only
/// when it is unchecked and the focused pane has no group.
pub(crate) fn broadcast_rows(
    default: kettle_config::BroadcastDefault,
    kind: BroadcastKind,
    pane_group: bool,
) -> [BroadcastRow; 4] {
    use kettle_config::BroadcastDefault as D;
    let row = |kind, action, checked, enabled| BroadcastRow {
        kind,
        action,
        checked,
        enabled,
    };
    let group_checked = matches!(
        kind,
        BroadcastKind::FocusedGroup | BroadcastKind::OtherGroup
    );
    [
        row(
            BroadcastRowKind::Off,
            Action::ToggleBroadcastOff,
            kind == BroadcastKind::Off,
            true,
        ),
        row(
            BroadcastRowKind::Tab,
            if default == D::Group {
                Action::ToggleBroadcastAll
            } else {
                Action::ToggleBroadcastPanesInTab
            },
            kind == BroadcastKind::Tab,
            true,
        ),
        row(
            BroadcastRowKind::Window,
            if default == D::All {
                Action::ToggleBroadcastAll
            } else {
                Action::ToggleBroadcastWindow
            },
            kind == BroadcastKind::Window,
            true,
        ),
        row(
            BroadcastRowKind::Group,
            if kind == BroadcastKind::OtherGroup {
                Action::ToggleBroadcastOff
            } else {
                Action::ToggleBroadcastGroup
            },
            group_checked,
            group_checked || pane_group,
        ),
    ]
}

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

    /// Run a broadcast action on `ws`. False when it had nothing to do: a
    /// group toggle while the focused pane has no group, which then skips the
    /// rest of the action's dispatch as it always has.
    pub(super) fn apply_broadcast_action(&self, ws: &mut WindowState, action: &Action) -> bool {
        let focused_group = ws.mux.focused().and_then(|p| p.group_name.clone());
        match broadcast_scope_after(
            action,
            &ws.mux.broadcast,
            self.cfg.broadcast_default,
            focused_group.as_deref(),
        ) {
            Some(scope) => {
                self.set_broadcast_scope(ws, scope);
                true
            }
            None => {
                if matches!(action, Action::ToggleBroadcastGroup) {
                    log::info!(
                        "toggle-broadcast-group: focused pane has no group_name; \
                         use Action::CreateGroup or Action::GroupTab first"
                    );
                }
                false
            }
        }
    }
}

#[cfg(test)]
#[path = "broadcast_tests.rs"]
mod tests;

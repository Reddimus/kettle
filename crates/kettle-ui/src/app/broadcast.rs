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
//!
//! [`BroadcastRoles`] says which visible panes the input reaches: the frame's
//! outlines, the screen-reader descriptions and `ui_geometry` all read it.
use super::*;

/// The panes broadcast input reaches in one window: its sending pane and the
/// panes that receive. Derived from the scope's targets
/// (`Mux::broadcast_target_ids`, `compute_broadcast_targets`) each time it
/// is read, so focus moves, splits, closes, zoom and scope changes need no
/// bookkeeping of their own. A target that refuses input
/// (`Pane::input_refusal`: read-only, held after its shell exited, or its PTY
/// input failed) is not a receiver, since delivery skips it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct BroadcastRoles {
    sender: Option<u64>,
    receivers: HashSet<u64>,
}

impl BroadcastRoles {
    /// `ws`'s roles. While its own scope is on, its focused pane sends and
    /// every other target that takes input receives, whether or not `ws` is
    /// the focused window: its scope is armed, and typing there broadcasts as
    /// soon as it has the focus. `focused_window_scope` is the scope of the
    /// window the user types in, when that is another window. Only a named
    /// group crosses windows (`App::broadcast_input`), so only then do this
    /// window's members of that group receive too.
    pub(crate) fn of_window(
        ws: &WindowState,
        focused_window_scope: Option<&BroadcastScope>,
    ) -> Self {
        let mut roles = Self::default();
        if ws.mux.is_broadcast_on() {
            roles.sender = ws.mux.active_focus();
            roles
                .receivers
                .extend(accepting(ws, ws.mux.broadcast_target_ids()));
        }
        if let Some(scope) = focused_window_scope {
            roles.receivers.extend(foreign_receivers(ws, scope));
        }
        if let Some(sender) = roles.sender {
            roles.receivers.remove(&sender);
        }
        roles
    }

    pub(crate) fn of(&self, pane: u64) -> kettle_render::PaneBroadcast {
        if self.sender == Some(pane) {
            kettle_render::PaneBroadcast::Sending
        } else if self.receivers.contains(&pane) {
            kettle_render::PaneBroadcast::Receiving
        } else {
            kettle_render::PaneBroadcast::Off
        }
    }
}

/// The panes of `ids` in `ws` that take user input.
fn accepting(ws: &WindowState, ids: Vec<u64>) -> impl Iterator<Item = u64> + '_ {
    ids.into_iter().filter(|id| {
        ws.mux
            .panes
            .get(id)
            .is_some_and(|pane| pane.input_refusal().is_none())
    })
}

/// The panes of `ws` that input typed in another window under `scope`
/// reaches: the members of a named group that take input. Nothing for a
/// scope that stays inside its window.
fn foreign_receivers<'a>(
    ws: &'a WindowState,
    scope: &BroadcastScope,
) -> impl Iterator<Item = u64> + 'a {
    let ids = if crate::mux::Mux::scope_crosses_windows(scope) {
        ws.mux.foreign_target_ids(scope)
    } else {
        Vec::new()
    };
    accepting(ws, ids)
}

/// What the focused window's group broadcast reaches in the other windows,
/// as a fingerprint: the focused window, its group, and each other window's
/// receiving panes. `None` while the focused window's scope stays inside it,
/// which costs one lookup. Any change to it (a group scope set or cleared,
/// the focus moving to another window, a pane joining or leaving the group,
/// a member closing, exiting, turning read-only or moving to another
/// window, a window closing) leaves the other windows' outlines stale, so
/// `App::sync_foreign_broadcast_reach` redraws them.
pub(crate) fn foreign_broadcast_reach(
    focused_seq: u64,
    windows: &std::collections::BTreeMap<u64, WindowState>,
) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let scope = &windows.get(&focused_seq)?.mux.broadcast;
    // Only a named group crosses windows (`Mux::scope_crosses_windows`).
    let BroadcastScope::Group(group) = scope else {
        return None;
    };
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    focused_seq.hash(&mut hasher);
    group.hash(&mut hasher);
    for (seq, window) in windows.iter().filter(|(seq, _)| **seq != focused_seq) {
        let mut members: Vec<u64> = foreign_receivers(window, scope).collect();
        members.sort_unstable();
        (seq, members).hash(&mut hasher);
    }
    Some(hasher.finish())
}

/// The scope of the window the user types in, for drawing `target_seq`'s
/// panes; `None` when that is `target_seq` itself. The window being
/// dispatched, `in_hand`, is out of `App::windows` while it is, so it is
/// asked first: a `ui_geometry` read of another window runs while the
/// focused window is in hand. `others` looks up the rest.
pub(crate) fn focused_window_scope<'a>(
    focused_seq: u64,
    target_seq: u64,
    in_hand: &'a WindowState,
    others: impl FnOnce(u64) -> Option<&'a WindowState>,
) -> Option<&'a BroadcastScope> {
    if focused_seq == target_seq {
        return None;
    }
    let focused = if in_hand.seq == focused_seq {
        Some(in_hand)
    } else {
        others(focused_seq)
    };
    focused.map(|window| &window.mux.broadcast)
}

/// `target`'s roles while `in_hand` is the window being dispatched and
/// `windows` holds the rest: what [`App::broadcast_roles_beside`] reads, for
/// the frame, the AccessKit tree and `ui_geometry`.
pub(crate) fn roles_beside(
    target: &WindowState,
    in_hand: &WindowState,
    focused_seq: u64,
    windows: &std::collections::BTreeMap<u64, WindowState>,
) -> BroadcastRoles {
    let scope = focused_window_scope(focused_seq, target.seq, in_hand, |seq| windows.get(&seq));
    BroadcastRoles::of_window(target, scope)
}

/// What a screen reader adds after a terminal pane's name while broadcast
/// reaches it; nothing otherwise. A description, not a live region, so a
/// scope change re-describes the panes without announcing each one.
pub(crate) fn pane_broadcast_description(
    role: kettle_render::PaneBroadcast,
    tr: &kettle_i18n::Translator,
) -> Option<&'static str> {
    match role {
        kettle_render::PaneBroadcast::Off => None,
        kettle_render::PaneBroadcast::Sending => {
            Some(tr.text(kettle_i18n::Text::PaneA11yBroadcastSending))
        }
        kettle_render::PaneBroadcast::Receiving => {
            Some(tr.text(kettle_i18n::Text::PaneA11yBroadcastReceiving))
        }
    }
}

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

    /// `ws`'s [`BroadcastRoles`] while it is the window in hand, with the
    /// scope of the focused window when that is another one.
    pub(super) fn broadcast_roles(&self, ws: &WindowState) -> BroadcastRoles {
        self.broadcast_roles_beside(ws, ws)
    }

    /// `target`'s [`BroadcastRoles`] while `in_hand` is the window being
    /// dispatched; `target` may be another window, read from `self.windows`.
    pub(super) fn broadcast_roles_beside(
        &self,
        target: &WindowState,
        in_hand: &WindowState,
    ) -> BroadcastRoles {
        roles_beside(target, in_hand, self.focused_seq, &self.windows)
    }

    /// Request a frame from every window not in hand.
    pub(super) fn redraw_other_windows(&self) {
        for other in self.windows.values() {
            if let Some(window) = &other.window {
                window.request_redraw();
            }
        }
    }

    /// Redraw every window when what the focused window's group broadcast
    /// reaches in the other windows changed ([`foreign_broadcast_reach`]).
    /// Run once per event-loop turn with every window back in
    /// `self.windows`, so one check covers each way the reach can change.
    pub(super) fn sync_foreign_broadcast_reach(&mut self) {
        let reach = foreign_broadcast_reach(self.focused_seq, &self.windows);
        if reach != self.foreign_broadcast_reach {
            self.foreign_broadcast_reach = reach;
            self.redraw_other_windows();
        }
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

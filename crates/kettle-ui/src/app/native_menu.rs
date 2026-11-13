//! Snapshot only the facts needed to validate the focused window's menu.
use super::*;

impl App {
    pub(super) fn sync_macos_menu(&self, _ws: Option<&WindowState>) {
        #[cfg(target_os = "macos")]
        {
            let ws = _ws.or_else(|| self.windows.get(&self.focused_seq));
            let pane = ws.and_then(|ws| ws.mux.active_focus().and_then(|id| ws.mux.panes.get(&id)));
            let broadcast = ws.map(BroadcastKind::of_window).unwrap_or_default();
            let context =
                crate::macos_menu::Context {
                    window: ws.is_some_and(|ws| ws.window.is_some()),
                    pane: pane.is_some(),
                    tabs: ws.map_or(0, |ws| ws.mux.tabs.len()),
                    panes: ws.map_or(0, |ws| ws.mux.active_pane_count()),
                    zoomed: ws.is_some_and(|ws| ws.mux.is_zoomed()),
                    clipboard: self.clipboard.is_some(),
                    selection: pane.is_some_and(|pane| {
                        pane.term.term.lock().ok().is_some_and(|term| {
                            term.selection.as_ref().is_some_and(|s| !s.is_empty())
                        })
                    }),
                    smart_copy: self.cfg.smart_copy,
                    writable: ws.is_some_and(|ws| paste_is_writable(ws, self.windows.values())),
                    renderer: ws.is_some_and(|ws| ws.renderer.is_some()),
                    detachable_tabs: self.cfg.detachable_tabs,
                    fullscreen: ws
                        .and_then(|ws| ws.window.as_ref())
                        .is_some_and(|window| window.fullscreen().is_some()),
                    broadcast: broadcast.0,
                    pane_group: broadcast.1,
                    broadcast_default: self.cfg.broadcast_default,
                };
            crate::macos_menu::sync(&self.proxy, &self.cfg, context, &self.profile_inventory);
        }
    }
}

#[cfg(any(target_os = "macos", test))]
pub(super) fn paste_is_writable<'a>(
    ws: &WindowState,
    other_windows: impl Iterator<Item = &'a WindowState>,
) -> bool {
    let writable = |pane: &Pane| !pane.read_only && !pane.pty_input_failed();
    if !ws.mux.is_broadcast_on() {
        return ws
            .mux
            .active_focus()
            .and_then(|id| ws.mux.panes.get(&id))
            .is_some_and(writable);
    }
    let any_writable = |mux: &Mux, targets: Vec<u64>| {
        targets
            .into_iter()
            .any(|id| mux.panes.get(&id).is_some_and(writable))
    };
    any_writable(&ws.mux, ws.mux.broadcast_target_ids())
        || (Mux::scope_crosses_windows(&ws.mux.broadcast)
            && other_windows
                .filter(|other| other.seq != ws.seq)
                .any(|other| {
                    any_writable(&other.mux, other.mux.foreign_target_ids(&ws.mux.broadcast))
                }))
}

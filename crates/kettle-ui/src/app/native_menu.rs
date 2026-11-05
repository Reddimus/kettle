//! Snapshot only the facts needed to validate the focused window's menu.
use super::*;

impl App {
    pub(super) fn sync_macos_menu(&self, _ws: Option<&WindowState>) {
        #[cfg(target_os = "macos")]
        {
            let ws = _ws.or_else(|| self.windows.get(&self.focused_seq));
            let pane = ws.and_then(|ws| ws.mux.active_focus().and_then(|id| ws.mux.panes.get(&id)));
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
                    writable: pane.is_some_and(|pane| !pane.read_only && !pane.pty_input_failed()),
                    renderer: ws.is_some_and(|ws| ws.renderer.is_some()),
                };
            crate::macos_menu::sync(&self.proxy, &self.cfg, context);
        }
    }
}

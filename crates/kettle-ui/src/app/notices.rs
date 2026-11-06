//! Persistent notices share the toast painter without changing terminal geometry.
use super::*;

pub(super) fn config_notice_message(
    path: &std::path::Path,
    entries: &[kettle_config::ConfigDiagnostic],
    tr: &kettle_i18n::Translator,
) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let file: String = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    let mut lines: Vec<_> = entries
        .iter()
        .take(3)
        .map(|entry| {
            let key: String = entry
                .key
                .chars()
                .filter(|c| !c.is_control())
                .take(64)
                .collect();
            format!("{file}:{}: {key}", entry.line)
        })
        .collect();
    if entries.len() > 3 {
        lines.push(tr.config_notice_more((entries.len() - 3) as u64));
    }
    Some(tr.config_notice(&lines.join("\n")))
}

impl App {
    pub(super) fn set_config_notice(
        &mut self,
        path: &std::path::Path,
        entries: &[kettle_config::ConfigDiagnostic],
    ) {
        let message = config_notice_message(path, entries, &self.ui_text);
        if message != self.config_notice_last {
            self.config_notice = message.clone();
            self.config_notice_last = message;
            for ws in self.windows.values_mut() {
                ws.accessibility_pending = true;
                if let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
        }
    }

    /// None is the window's config notice; Some(id) is that pane's exit banner.
    pub(super) fn persistent_notices(
        &self,
        ws: &WindowState,
    ) -> Vec<(Option<u64>, kettle_render::UpdateToastOverlay)> {
        if self.pointer_modal_open(ws) {
            return Vec::new();
        }
        let cell = (
            self.overlay_text_cell_width(ws),
            self.overlay_text_line_height(ws),
        );
        let mut notices = Vec::new();
        for (id, partition) in self.pane_layout(ws, self.area(ws)) {
            let Some(pane) = ws
                .mux
                .panes
                .get(&id)
                .filter(|pane| pane.held || pane.osc52_notice.message.is_some())
            else {
                continue;
            };
            let (message, details, close) = if pane.held {
                (
                    pane.exit_code
                        .map(|code| self.ui_text.process_exited(u64::from(code)))
                        .unwrap_or_else(|| {
                            self.ui_text
                                .text(kettle_i18n::Text::ProcessExitPending)
                                .into()
                        }),
                    self.ui_text
                        .text(kettle_i18n::Text::ProcessRestart)
                        .to_string(),
                    self.ui_text
                        .text(kettle_i18n::Text::ProcessClose)
                        .to_string(),
                )
            } else {
                (
                    pane.osc52_notice.message.clone().unwrap_or_default(),
                    String::new(),
                    self.ui_text
                        .text(kettle_i18n::Text::NoticeDismiss)
                        .to_string(),
                )
            };
            let columns = ((partition.terminal.2 - 12.0) / cell.0).floor().max(2.0) as usize - 1;
            let rows = if pane.held {
                2
            } else {
                kettle_render::toast_text_rows(&message, columns).unwrap_or(1)
            };
            if let Some(geometry) = kettle_render::persistent_notice_geometry(
                partition.terminal,
                cell,
                rows,
                details.width(),
                close.width(),
            ) {
                notices.push((
                    Some(id),
                    kettle_render::UpdateToastOverlay {
                        geometry,
                        message,
                        details,
                        close,
                        opacity: 1.0,
                        focused: None,
                    },
                ));
            }
        }
        if let Some(message) = &self.config_notice {
            let area = self.area(ws);
            let width = area.2.min(760.0);
            let rect = (area.0 + (area.2 - width) * 0.5, area.1, width, area.3);
            let close = self
                .ui_text
                .text(kettle_i18n::Text::NoticeDismiss)
                .to_string();
            let columns = ((width - 12.0) / cell.0).floor().max(2.0) as usize - 1;
            let rows = message
                .lines()
                .map(|line| line.width().div_ceil(columns).max(1))
                .sum();
            if let Some(geometry) =
                kettle_render::persistent_notice_geometry(rect, cell, rows, 0, close.width())
            {
                notices.retain(|(_, toast)| {
                    let a = toast.geometry.rect;
                    let b = geometry.rect;
                    a.0 + a.2 <= b.0 || b.0 + b.2 <= a.0 || a.1 + a.3 <= b.1 || b.1 + b.3 <= a.1
                });
                notices.push((
                    None,
                    kettle_render::UpdateToastOverlay {
                        geometry,
                        message: message.clone(),
                        details: String::new(),
                        close,
                        opacity: 1.0,
                        focused: None,
                    },
                ));
            }
        }
        notices
    }

    pub(super) fn activate_persistent_notice(
        &mut self,
        ws: &mut WindowState,
        pane: Option<u64>,
        restart: bool,
    ) {
        if let Some(id) = pane {
            let Some(pane) = ws.mux.panes.get_mut(&id) else {
                return;
            };
            if !pane.held {
                if !restart {
                    pane.osc52_notice.message = None;
                }
            } else if restart {
                let rect = self
                    .pane_layout(ws, self.area(ws))
                    .into_iter()
                    .find(|(pane, _)| *pane == id)
                    .map(|(_, p)| p.terminal);
                let Some(rect) = rect else {
                    return;
                };
                let (cols, rows) = self.grid_of(ws, rect);
                let geometry = self.pty_geometry_for_grid(ws, cols, rows);
                match ws.mux.restart_held(id, &self.cfg, geometry, self.waker()) {
                    Ok(true) => {
                        self.close_preview(ws, id);
                        self.media.cards.retire(|card| card.pane == id);
                        ws.seen_output_gen.remove(&id);
                        ws.pane_snapshots.clear();
                        ws.pane_snapshot_keys.clear();
                        ws.reuse_pane_snapshots_once = false;
                        self.resize_all(ws);
                    }
                    Ok(false) => {}
                    Err(error) => log::warn!("cannot restart held pane {id}: {error}"),
                }
            } else if let Some(pane) = ws.mux.panes.get_mut(&id) {
                pane.closed = true;
            }
        } else {
            self.config_notice = None;
            for window in self.windows.values_mut() {
                window.accessibility_pending = true;
                if let Some(native) = &window.window {
                    native.request_redraw();
                }
            }
        }
        ws.accessibility_pending = true;
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
    }

    pub(super) fn press_persistent_notice(
        &mut self,
        ws: &mut WindowState,
        x: f32,
        y: f32,
        button: u8,
    ) -> bool {
        let target = self
            .persistent_notices(ws)
            .into_iter()
            .rev()
            .find_map(|(pane, toast)| toast.geometry.hit_test(x, y).map(|hit| (pane, hit)));
        let Some((pane, hit)) = target else {
            return false;
        };
        if button == 0 {
            match hit {
                kettle_render::UpdateToastHit::Close => {
                    self.activate_persistent_notice(ws, pane, false)
                }
                kettle_render::UpdateToastHit::Details if pane.is_some() => {
                    self.activate_persistent_notice(ws, pane, true)
                }
                _ => {}
            }
        }
        true
    }
}

pub(super) fn held_pane_key(ws: &WindowState, key: &Key) -> Option<(u64, bool)> {
    let id = ws.mux.active_focus()?;
    let pane = ws.mux.panes.get(&id)?;
    if !pane.held {
        return None;
    }
    held_process_key(key, ws.mods).map(|restart| (id, restart))
}

pub(super) fn held_process_key(key: &Key, mods: ModifiersState) -> Option<bool> {
    if !mods.is_empty() {
        return None;
    }
    match key {
        Key::Named(NamedKey::Enter) => Some(true),
        Key::Named(NamedKey::Escape) => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_notice_names_three_source_locations_and_counts_the_rest_in_both_languages() {
        let entries: Vec<_> = (1..=5)
            .map(|line| kettle_config::ConfigDiagnostic {
                line,
                key: format!("key-{line}"),
            })
            .collect();
        for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
            let tr = kettle_i18n::Translator::new(language);
            assert!(config_notice_message(std::path::Path::new("config"), &[], &tr).is_none());
            let message =
                config_notice_message(std::path::Path::new("/fixture/config"), &entries, &tr)
                    .unwrap();
            assert!(message.contains("config:1: key-1"));
            assert!(message.contains("config:3: key-3"));
            assert!(!message.contains("key-4"));
            assert!(message.contains(&tr.config_notice_more(2)));
        }
    }
}

#[cfg(test)]
#[test]
fn held_process_keys_restart_or_close_only_for_unmodified_enter_and_escape() {
    assert_eq!(
        held_process_key(&Key::Named(NamedKey::Enter), ModifiersState::empty()),
        Some(true)
    );
    assert_eq!(
        held_process_key(&Key::Named(NamedKey::Escape), ModifiersState::empty()),
        Some(false)
    );
    assert_eq!(
        held_process_key(&Key::Character("x".into()), ModifiersState::empty()),
        None
    );
    for mods in [
        ModifiersState::CONTROL,
        ModifiersState::SHIFT,
        ModifiersState::ALT,
        ModifiersState::SUPER,
    ] {
        assert_eq!(held_process_key(&Key::Named(NamedKey::Enter), mods), None);
        assert_eq!(held_process_key(&Key::Named(NamedKey::Escape), mods), None);
    }
}

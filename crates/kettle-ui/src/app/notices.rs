//! Persistent notices share the toast painter without changing terminal geometry.
use super::*;

fn notice_token(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).take(64).collect()
}

#[derive(Debug, PartialEq, Eq)]
enum PersistentNoticeAction {
    OpenConfig(Option<std::path::PathBuf>),
    DismissConfig,
    Pane(u64, bool),
}

fn persistent_notice_action(
    pane: Option<u64>,
    primary: bool,
    config_path: Option<&std::path::Path>,
) -> PersistentNoticeAction {
    match (pane, primary) {
        (Some(id), primary) => PersistentNoticeAction::Pane(id, primary),
        (None, true) => PersistentNoticeAction::OpenConfig(config_path.map(ToOwned::to_owned)),
        (None, false) => PersistentNoticeAction::DismissConfig,
    }
}

pub(super) fn config_notice_message(
    path: &std::path::Path,
    entries: &[kettle_config::ConfigDiagnostic],
    tr: &kettle_i18n::Translator,
) -> Option<String> {
    if entries.is_empty() {
        return None;
    }
    let file = notice_token(
        &path
            .file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy(),
    );
    let mut lines: Vec<_> = entries
        .iter()
        .take(3)
        .map(|entry| {
            use kettle_config::{ConfigDiagnosticKind as Kind, ConfigValueForm as Form};
            use kettle_i18n::Text as T;
            let key = notice_token(&entry.key);
            let reason = match &entry.kind {
                Kind::UnknownKey {
                    suggestion: Some(key),
                } => tr.config_notice_suggestion(&notice_token(key)),
                Kind::UnknownKey { suggestion: None } => tr.text(T::ConfigNoticeUnknown).into(),
                Kind::InvalidValue {
                    expected: Some(Form::NumberRange { min, max }),
                } => tr.config_notice_number(min, max),
                Kind::InvalidValue {
                    expected: Some(Form::Boolean),
                } => tr.text(T::ConfigNoticeBoolean).into(),
                Kind::InvalidValue { expected: None } => tr.text(T::ConfigNoticeInvalid).into(),
                Kind::MalformedLine {
                    missing_equals: true,
                } => tr.text(T::ConfigNoticeMissingEquals).into(),
                Kind::MalformedLine {
                    missing_equals: false,
                } => tr.text(T::ConfigNoticeMalformed).into(),
            };
            format!("{file}:{}  {key}: {reason}", entry.line)
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
        if message != self.config_notice_last || self.config_notice_path.as_deref() != Some(path) {
            self.config_notice_path = message.as_ref().map(|_| path.to_path_buf());
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
            let mouse_hint = ws.mouse_override_hint.pane() == Some(id);
            let Some(pane) = ws
                .mux
                .panes
                .get(&id)
                .filter(|pane| pane.held || pane.osc52_notice.message.is_some() || mouse_hint)
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
                    pane.osc52_notice.message.clone().unwrap_or_else(|| {
                        self.ui_text
                            .text(kettle_i18n::Text::MouseOverrideHint)
                            .into()
                    }),
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
            // The exit banner sits under the pane's output; a short session's
            // last lines are at the top, where the banner would hide them.
            let place = if pane.held {
                kettle_render::persistent_notice_geometry_at_bottom
            } else {
                kettle_render::persistent_notice_geometry
            };
            if let Some(geometry) = place(
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
            let details = self
                .ui_text
                .text(kettle_i18n::Text::ConfigNoticeOpen)
                .to_string();
            let columns = ((width - 12.0) / cell.0).floor().max(2.0) as usize - 1;
            let rows = kettle_render::toast_text_rows(message, columns).unwrap_or(1);
            if let Some(geometry) = kettle_render::persistent_notice_geometry(
                rect,
                cell,
                rows,
                details.width(),
                close.width(),
            ) {
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
                        details,
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
        primary: bool,
    ) {
        match persistent_notice_action(pane, primary, self.config_notice_path.as_deref()) {
            PersistentNoticeAction::Pane(id, primary) => {
                let Some(pane) = ws.mux.panes.get_mut(&id) else {
                    return;
                };
                if !pane.held {
                    // Dismiss what the notice showed: the clipboard notice
                    // comes before the Shift hint.
                    if !primary && pane.osc52_notice.message.take().is_none() {
                        ws.mouse_override_hint.dismiss();
                    }
                } else if primary {
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
            }
            PersistentNoticeAction::OpenConfig(path) => self.open_config_file(path),
            PersistentNoticeAction::DismissConfig => {
                self.config_notice = None;
                for window in self.windows.values_mut() {
                    window.accessibility_pending = true;
                    if let Some(native) = &window.window {
                        native.request_redraw();
                    }
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
            .find_map(|(pane, toast)| {
                toast
                    .control_geometry()
                    .hit_test(x, y)
                    .map(|hit| (pane, hit))
            });
        let Some((pane, hit)) = target else {
            return false;
        };
        if button == 0 {
            match hit {
                kettle_render::UpdateToastHit::Close => {
                    self.activate_persistent_notice(ws, pane, false)
                }
                kettle_render::UpdateToastHit::Details => {
                    self.activate_persistent_notice(ws, pane, true)
                }
                _ => {}
            }
        }
        true
    }
}

/// The config notice's answer to a key press: `Some(true)` opens the config,
/// `Some(false)` dismisses it. Only while the notice has focus (a screen
/// reader moved it there): the terminal is live under it, so a plain Enter
/// or Escape otherwise belongs to the shell or the program running in it.
pub(super) fn config_notice_key(
    visible: bool,
    focused: bool,
    key: &Key,
    mods: ModifiersState,
) -> Option<bool> {
    (visible && focused)
        .then(|| held_process_key(key, mods))
        .flatten()
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
    fn config_notice_formats_reasons_in_both_languages_without_value_text() {
        let path = std::path::Path::new("/fixture/kettle.cfg");
        let secret = "FAKE_SECRET_do_not_display";
        let text = format!("font-size = {secret}\nfont-sise = {secret}\nfont-size {secret}\n");
        let loaded = kettle_config::LoadedConfig {
            config: Config::parse_text(&text),
            unknown_keys: Config::parse_collect(&text).1,
            malformed_values: Config::detect_malformed_values(&text),
            text,
        };
        for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
            let tr = kettle_i18n::Translator::new(language);
            let message = config_notice_message(path, &loaded.diagnostics(), &tr).unwrap();
            assert!(message.contains(&format!(
                "kettle.cfg:1  font-size: {}",
                tr.config_notice_number("5", "72")
            )));
            assert!(message.contains(&format!(
                "kettle.cfg:2  font-sise: {}",
                tr.config_notice_suggestion("font-size")
            )));
            assert!(message.contains(&format!(
                "kettle.cfg:3  font-size: {}",
                tr.text(kettle_i18n::Text::ConfigNoticeMissingEquals)
            )));
            assert!(!message.contains(secret));
            assert!(
                !tr.notify_body_config_values_ignored(&message)
                    .contains(secret)
            );
        }
    }

    #[test]
    fn config_notice_filters_controls_and_caps_each_untrusted_token() {
        let file = format!("{}\n\tTAIL", "f".repeat(65));
        let entries = [kettle_config::ConfigDiagnostic {
            line: 42,
            key: format!("\u{1b}{}\r\nTAIL", "é".repeat(65)),
            kind: kettle_config::ConfigDiagnosticKind::UnknownKey {
                suggestion: Some("font-size"),
            },
        }];
        let tr = kettle_i18n::Translator::default();
        let message = config_notice_message(std::path::Path::new(&file), &entries, &tr).unwrap();
        assert!(message.contains(&format!("{}:42  {}: ", "f".repeat(64), "é".repeat(64))));
        assert!(!message.contains("TAIL"));
        assert!(!message.contains('\r'));
        assert!(!message.contains('\t'));
        assert!(!message.contains('\u{1b}'));
        assert_eq!(message.lines().count(), 2);
    }

    #[test]
    fn config_notice_actions_open_the_noticed_path_or_dismiss_and_preserve_pane_routing() {
        let path = std::path::Path::new("/fixture/profile with spaces.config");
        assert_eq!(
            persistent_notice_action(None, true, Some(path)),
            PersistentNoticeAction::OpenConfig(Some(path.into()))
        );
        assert_eq!(
            persistent_notice_action(None, false, Some(path)),
            PersistentNoticeAction::DismissConfig
        );
        for primary in [false, true] {
            assert_eq!(
                persistent_notice_action(Some(7), primary, Some(path)),
                PersistentNoticeAction::Pane(7, primary)
            );
        }
        let none = ModifiersState::empty();
        let (enter, escape) = (Key::Named(NamedKey::Enter), Key::Named(NamedKey::Escape));
        // Focused (a screen reader moved there): Enter opens, Escape dismisses.
        assert_eq!(config_notice_key(true, true, &enter, none), Some(true));
        assert_eq!(config_notice_key(true, true, &escape, none), Some(false));
        // Merely visible over a live terminal: the shell gets both keys, so a
        // command typed before the notice was noticed still runs.
        assert_eq!(config_notice_key(true, false, &enter, none), None);
        assert_eq!(config_notice_key(true, false, &escape, none), None);
        assert_eq!(config_notice_key(false, true, &enter, none), None);
        assert_eq!(
            config_notice_key(true, true, &enter, ModifiersState::CONTROL),
            None
        );
        assert_eq!(
            config_notice_key(true, true, &Key::Character("x".into()), none),
            None
        );
    }

    #[test]
    fn config_notice_names_three_source_locations_and_counts_the_rest_in_both_languages() {
        let entries: Vec<_> = (1..=5)
            .map(|line| kettle_config::ConfigDiagnostic {
                line,
                key: format!("key-{line}"),
                kind: kettle_config::ConfigDiagnosticKind::UnknownKey { suggestion: None },
            })
            .collect();
        for language in [kettle_i18n::Language::En, kettle_i18n::Language::Es] {
            let tr = kettle_i18n::Translator::new(language);
            assert!(config_notice_message(std::path::Path::new("config"), &[], &tr).is_none());
            let message =
                config_notice_message(std::path::Path::new("/fixture/config"), &entries, &tr)
                    .unwrap();
            assert!(message.contains("config:1  key-1: "));
            assert!(message.contains("config:3  key-3: "));
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

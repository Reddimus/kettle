use super::*;
#[cfg(test)]
use kettle_i18n::Language;
use kettle_i18n::Translator;
use std::time::{Duration, Instant};

#[derive(Default)]
pub(super) struct SelectionHint {
    used: bool,
    owner: Option<(u64, u64)>,
    until: Option<Instant>,
}

impl SelectionHint {
    fn observe(
        &mut self,
        owner: (u64, Option<u64>),
        eligible: bool,
        available: bool,
        selected: bool,
        dragging: bool,
        now: Instant,
    ) -> bool {
        let (window, pane) = owner;
        let before = (self.owner, self.until);
        if self.owner.is_some_and(|(owner, _)| owner == window)
            && (!available || !selected || self.owner.map(|(_, id)| id) != pane)
        {
            self.owner = None;
            self.until = None;
        }
        if !self.used
            && eligible
            && available
            && selected
            && let Some(pane) = pane
        {
            self.used = true;
            self.owner = Some((window, pane));
        }
        if self.owner.is_some_and(|(owner, _)| owner == window) {
            if !dragging && self.until.is_none() {
                self.until = Some(now + Duration::from_secs(8));
            }
            if self.until.is_some_and(|until| now >= until) {
                self.owner = None;
                self.until = None;
            }
        }
        before != (self.owner, self.until)
    }

    pub(super) fn dismiss(&mut self, window: u64) -> bool {
        if self.owner.is_some_and(|(owner, _)| owner == window) {
            self.owner = None;
            self.until = None;
            true
        } else {
            false
        }
    }
}

pub(super) fn selection_gesture_text(tr: &Translator, macos: bool) -> String {
    tr.selection_gesture_hint(if macos { "Option" } else { "Alt" })
}

pub(super) fn plain_selection_hint_gesture(
    clicks: u8,
    held: ModifiersState,
    selecting: bool,
) -> bool {
    clicks == 1 && held.is_empty() && selecting
}

impl App {
    pub(super) fn dismiss_selection_hint(&mut self, ws: &mut WindowState) {
        if self.selection_hint.dismiss(ws.seq) {
            ws.accessibility_pending = true;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
    }

    pub(super) fn sync_selection_hint(
        &mut self,
        ws: &mut WindowState,
        now: Instant,
    ) -> Option<Instant> {
        if self.selection_hint.used && self.selection_hint.owner.is_none() {
            return None;
        }
        let eligible = ws.selection_hint_plain && ws.selecting && ws.selection_dragged;
        if !eligible && self.selection_hint.owner.is_none() {
            return None;
        }
        let available = ws.window_focused && terminal_surface_available(ws);
        let selected = available && focused_selection_range(ws).is_some();
        if self.selection_hint.observe(
            (ws.seq, ws.mux.active_focus()),
            eligible,
            available,
            selected,
            ws.selecting,
            now,
        ) {
            ws.accessibility_pending = true;
            if let Some(window) = &ws.window {
                window.request_redraw();
            }
        }
        self.selection_hint
            .owner
            .filter(|(window, _)| *window == ws.seq)
            .and(self.selection_hint.until)
    }

    pub(super) fn selection_hint_status(
        &self,
        ws: &WindowState,
        pane: Rect,
        cell: (f32, f32),
        now: Instant,
    ) -> Option<kettle_render::PaneStatus> {
        if self.selection_hint.owner != ws.mux.active_focus().map(|pane| (ws.seq, pane))
            || !ws.window_focused
            || !terminal_surface_available(ws)
            || self.selection_hint.until.is_some_and(|until| now >= until)
            || focused_selection_range(ws).is_none()
        {
            return None;
        }
        kettle_render::selection_hint_layout(
            pane,
            cell,
            &selection_gesture_text(&self.ui_text, cfg!(target_os = "macos")),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c09_selection_hint_is_once_per_run_and_follows_its_gesture() {
        assert!(plain_selection_hint_gesture(
            1,
            ModifiersState::empty(),
            true
        ));
        for held in [
            ModifiersState::SHIFT,
            ModifiersState::ALT,
            ModifiersState::CONTROL,
            ModifiersState::SUPER,
        ] {
            assert!(!plain_selection_hint_gesture(1, held, true));
        }
        for clicks in [0, 2, 3] {
            assert!(!plain_selection_hint_gesture(
                clicks,
                ModifiersState::empty(),
                true
            ));
        }
        assert!(!plain_selection_hint_gesture(
            1,
            ModifiersState::empty(),
            false
        ));
        let now = Instant::now();
        let mut hint = SelectionHint::default();
        assert!(!hint.observe((1, Some(7)), false, true, true, true, now));
        assert!(!hint.observe((1, Some(7)), true, true, false, true, now));
        assert!(hint.observe((1, Some(7)), true, true, true, true, now));
        assert_eq!(hint.owner, Some((1, 7)));
        assert!(!hint.observe(
            (1, Some(7)),
            true,
            true,
            true,
            true,
            now + Duration::from_secs(30)
        ));
        assert!(hint.until.is_none(), "a held drag never expires");
        let release = now + Duration::from_secs(30);
        assert!(hint.observe((1, Some(7)), false, true, true, false, release));
        assert_eq!(hint.until, Some(release + Duration::from_secs(8)));
        assert!(!hint.observe(
            (1, Some(7)),
            false,
            true,
            true,
            false,
            release + Duration::from_millis(7999)
        ));
        assert!(hint.observe(
            (1, Some(7)),
            false,
            true,
            true,
            false,
            release + Duration::from_secs(8)
        ));
        assert_eq!(hint.owner, None);
        assert!(!hint.observe(
            (2, Some(9)),
            true,
            true,
            true,
            true,
            release + Duration::from_secs(9)
        ));
        for (pane, available, selected) in [
            (Some(8), true, true),
            (Some(7), false, true),
            (Some(7), true, false),
        ] {
            let mut hint = SelectionHint::default();
            hint.observe((1, Some(7)), true, true, true, true, now);
            assert!(hint.observe((1, pane), false, available, selected, true, now));
            assert_eq!(hint.owner, None);
            assert!(!hint.observe((1, Some(7)), true, true, true, true, now));
        }
        let mut hint = SelectionHint::default();
        hint.observe((1, Some(7)), true, true, true, true, now);
        assert!(hint.dismiss(1));
        assert!(!hint.observe((1, Some(7)), true, true, true, true, now));
    }

    #[test]
    fn c09_selection_hint_localization_layout_and_accessible_status_agree() {
        for (language, macos, expected) in [
            (
                Language::En,
                false,
                "Alt-drag: rectangular selection · Shift-click: extend selection",
            ),
            (
                Language::En,
                true,
                "Option-drag: rectangular selection · Shift-click: extend selection",
            ),
            (
                Language::Es,
                false,
                "Alt + arrastrar: selección rectangular · Shift + clic: extender selección",
            ),
            (
                Language::Es,
                true,
                "Option + arrastrar: selección rectangular · Shift + clic: extender selección",
            ),
        ] {
            let tr = Translator::new(language);
            let text = selection_gesture_text(&tr, macos);
            assert_eq!(text, expected);
            let status =
                kettle_render::selection_hint_layout((0.0, 0.0, 800.0, 400.0), (8.0, 16.0), &text)
                    .unwrap();
            assert_eq!(status.description, text);
            assert_eq!(status.text.replace('\n', " · "), text);
            let overlay = Overlay {
                pane_statuses: vec![status],
                ..Overlay::default()
            };
            let projection =
                modal_accessibility_projection(&overlay, (800.0, 400.0), (8.0, 16.0), 1.0, &tr);
            let statuses: Vec<_> = projection
                .nodes
                .iter()
                .filter(|(_, node)| node.role() == Role::Status)
                .collect();
            assert_eq!(statuses.len(), 1);
            assert_eq!(statuses[0].1.label(), Some(text.as_str()));
            assert_eq!(statuses[0].1.live(), Some(accesskit::Live::Polite));
            assert!(!statuses[0].1.supports_action(accesskit::Action::Focus));
        }
    }
}

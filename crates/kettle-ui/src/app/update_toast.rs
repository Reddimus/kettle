//! Transient update overlays preserve PTY layout and ordinary keyboard input.

use super::*;
use crate::update_toast::{Claim, ClaimReply, Toast};

pub(super) fn owns_key(focused: bool, key: &Key, mods: ModifiersState) -> bool {
    focused
        && !mods.control_key()
        && !mods.alt_key()
        && !mods.super_key()
        && matches!(
            key,
            Key::Named(NamedKey::Escape | NamedKey::Tab | NamedKey::Enter | NamedKey::Space)
        )
}

pub(super) fn guard_activation(
    key: &Key,
    physical: PhysicalKey,
    closing: &mut HashSet<PhysicalKey>,
) {
    if matches!(
        key,
        Key::Named(NamedKey::Escape | NamedKey::Enter | NamedKey::Space)
    ) {
        closing.insert(physical);
    }
}

fn finish(claim: Claim, seen: bool) {
    let _ = std::thread::Builder::new()
        .name("kettle-toast-receipt".into())
        .spawn(move || {
            if !crate::update_check::finish_toast(&claim, seen) {
                log::warn!("update toast receipt was not persisted");
            }
        });
}

impl App {
    fn update_toast_message(&self, notice: &crate::update_notice::Notice) -> String {
        match notice.kind {
            crate::update_notice::NoticeKind::Available => {
                self.ui_text.update_fact_available(&notice.tag)
            }
            crate::update_notice::NoticeKind::RestartPending => {
                self.ui_text.update_toast_installed(&notice.tag)
            }
            crate::update_notice::NoticeKind::Failed => {
                self.ui_text.update_fact_failed(&notice.tag)
            }
        }
    }

    fn update_toast_geometry(
        &self,
        ws: &WindowState,
    ) -> Option<kettle_render::UpdateToastGeometry> {
        let surface = ws.renderer.as_ref()?.surface_size();
        let scale = ws.window.as_ref().map_or(1.0, |w| w.scale_factor() as f32);
        let details = self.ui_text.text(kettle_i18n::Text::UpdateToastDetails);
        let notice = self
            .update_toast
            .as_ref()
            .filter(|toast| toast.window == ws.seq)
            .map(|toast| toast.claim.notice.clone())
            .or_else(|| self.update_notice())?;
        kettle_render::update_toast_geometry(
            (surface.0 as f32, surface.1 as f32),
            self.area(ws).1,
            scale,
            Self::cell_size(ws)?,
            details.width(),
            &self.update_toast_message(&notice),
        )
    }

    pub(super) fn toast_visible(&self, ws: &WindowState) -> bool {
        ws.window_focused
            && ws.window_shown
            && !ws.window_occluded
            && ws
                .window
                .as_ref()
                .is_some_and(|w| w.is_minimized() != Some(true))
            && terminal_surface_available(ws)
            && !ws.search.open
            && !self.non_search_modal_open(ws)
            && self.update_toast_geometry(ws).is_some()
    }

    pub(super) fn update_toast_projection(
        &self,
        ws: &WindowState,
    ) -> Option<kettle_render::UpdateToastOverlay> {
        let toast = self.update_toast.as_ref().filter(|t| t.window == ws.seq)?;
        let expired_claim = !toast.acknowledged
            && !toast.claim.live_at(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            );
        if !self.toast_visible(ws) || toast.remaining().is_zero() || expired_claim {
            return None;
        }
        Some(kettle_render::UpdateToastOverlay {
            geometry: self.update_toast_geometry(ws)?,
            message: self.update_toast_message(&toast.claim.notice),
            details: self
                .ui_text
                .text(kettle_i18n::Text::UpdateToastDetails)
                .into(),
            close: "\u{00d7}".into(),
            focused: match self.update_toast_focus {
                Some(ACCESSIBILITY_UPDATE_DETAILS_ID) => Some(1),
                Some(ACCESSIBILITY_UPDATE_DISMISS_ID) => Some(2),
                _ => None,
            },
            opacity: toast.opacity(self.motion_allowed()),
        })
    }

    pub(super) fn receive_update_toast(
        &mut self,
        ws: &mut WindowState,
        window: u64,
        reply: ClaimReply,
    ) {
        let pending = self.update_toast_claiming.take();
        log::debug!(
            "update toast response: {reply:?}, pending {pending:?}, active window {}",
            ws.seq
        );
        let valid = pending
            .as_ref()
            .is_some_and(|(n, w)| *w == window && Some(n.clone()) == self.update_notice())
            && ws.seq == window
            && self.toast_visible(ws);
        match reply {
            ClaimReply::Acquired(claim) if valid => {
                self.update_toast = Some(Toast::new(claim, window, std::time::Instant::now()));
                self.sync_cursor_icon(ws);
                ws.accessibility_pending = true;
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            }
            ClaimReply::Acquired(claim) => finish(claim, false),
            ClaimReply::Unavailable => {
                self.update_toast_unavailable = pending.map(|(n, _)| n);
            }
            ClaimReply::Seen => {
                self.update_toast_handled = pending.map(|(n, _)| n);
            }
            ClaimReply::Busy(until) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                self.update_toast_retry = Some(
                    std::time::Instant::now()
                        + std::time::Duration::from_secs(until.saturating_sub(now).clamp(1, 30)),
                );
            }
        }
    }

    pub(super) fn update_toast_presented(&mut self, window: u64) {
        if let Some(toast) = self.update_toast.as_mut().filter(|t| t.window == window) {
            toast.presented = true;
            if !toast.acknowledged {
                toast.acknowledged = true;
                self.update_toast_handled = Some(toast.claim.notice.clone());
                if let Some(writer) = &self.update_toast_receipts {
                    writer.note(toast.claim.clone());
                }
            }
        }
    }

    pub(super) fn dismiss_update_toast(&mut self, ws: &mut WindowState) -> bool {
        let Some(toast) = self.update_toast.take() else {
            return false;
        };
        if toast.presented
            && !toast.acknowledged
            && let Some(writer) = &self.update_toast_receipts
        {
            writer.note(toast.claim.clone());
        }
        if !toast.presented {
            finish(toast.claim.clone(), false);
        }
        self.update_toast_handled = Some(toast.claim.notice);
        self.update_toast_focus = None;
        self.sync_cursor_icon(ws);
        ws.accessibility_pending = true;
        if let Some(w) = &ws.window {
            w.request_redraw();
        }
        true
    }

    pub(super) fn tick_update_toast(
        &mut self,
        ws: &mut WindowState,
        now: std::time::Instant,
    ) -> Option<std::time::Duration> {
        let visible = self.toast_visible(ws);
        let notice = self.update_notice();
        let hovered = self.update_toast_geometry(ws).is_some_and(|g| {
            ws.native_pointer
                .is_some_and(|p| rect_contains(g.rect, p.x as f32, p.y as f32))
        });
        let motion = self.motion_allowed();
        if let Some(toast) = self.update_toast.as_mut().filter(|t| t.window == ws.seq) {
            if !visible {
                self.update_toast_focus = None;
            }
            toast.tick(now, visible, hovered || self.update_toast_focus.is_some());
            let expired = toast.remaining().is_zero()
                || (!toast.acknowledged
                    && !toast.claim.live_at(
                        std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_secs(),
                    ));
            let changed = Some(toast.claim.notice.clone()) != notice;
            if expired || changed || (!toast.presented && !visible) {
                let toast = self.update_toast.take().expect("toast is present");
                self.update_toast_focus = None;
                if !toast.acknowledged {
                    finish(toast.claim, false);
                }
                self.sync_cursor_icon(ws);
                ws.accessibility_pending = true;
                if let Some(w) = &ws.window {
                    w.request_redraw();
                }
            } else {
                let wake = toast.wake_after(motion);
                if visible
                    && !hovered
                    && self.update_toast_focus.is_none()
                    && motion
                    && toast.remaining() <= std::time::Duration::from_millis(150)
                    && let Some(w) = &ws.window
                {
                    w.request_redraw();
                }
                return wake;
            }
        }
        if !visible || self.update_toast.is_some() || self.update_toast_claiming.is_some() {
            return None;
        }
        let notice = notice.filter(|n| {
            self.update_toast_handled.as_ref() != Some(n)
                && self.update_toast_unavailable.as_ref() != Some(n)
                && !self
                    .update_toast_receipts
                    .as_ref()
                    .is_some_and(|writer| writer.suppresses(n))
        })?;
        if self.update_toast_receipts.is_none() {
            match crate::update_toast::ReceiptWriter::start() {
                Ok(writer) => self.update_toast_receipts = Some(writer),
                Err(error) => {
                    log::warn!("update toast receipts unavailable: {error}");
                    self.update_toast_unavailable = Some(notice);
                    return None;
                }
            }
        }
        if let Some(at) = self.update_toast_retry.take().filter(|at| *at > now) {
            self.update_toast_retry = Some(at);
            return Some(at.saturating_duration_since(now));
        }
        log::debug!("claiming update toast {notice:?} for window {}", ws.seq);
        self.update_toast_claiming = Some((notice.clone(), ws.seq));
        let proxy = self.proxy.clone();
        let window = ws.seq;
        if std::thread::Builder::new()
            .name("kettle-toast-claim".into())
            .spawn(move || {
                let reply = crate::update_check::claim_toast(notice);
                let _ = proxy.send_event(UserEvent::UpdateToastClaimed { window, reply });
            })
            .is_err()
        {
            self.update_toast_unavailable =
                self.update_toast_claiming.take().map(|(notice, _)| notice);
            log::warn!("update toast delivery thread could not start");
        }
        None
    }

    pub(super) fn close_update_toast_window(&mut self, window: u64) {
        if self
            .update_toast
            .as_ref()
            .is_some_and(|toast| toast.window == window)
        {
            let toast = self.update_toast.take().expect("matching toast");
            if !toast.acknowledged {
                finish(toast.claim, false);
            }
            self.update_toast_focus = None;
        }
    }

    pub(super) fn press_update_toast(
        &mut self,
        ws: &mut WindowState,
        x: f32,
        y: f32,
        button: u8,
    ) -> bool {
        let Some(toast) = self.update_toast_projection(ws) else {
            return false;
        };
        if !rect_contains(toast.geometry.rect, x, y) {
            if self.update_toast_focus.take().is_some() {
                ws.accessibility_pending = true;
                if let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
            return false;
        }
        if button == 0 {
            if rect_contains(toast.geometry.close, x, y) {
                self.dismiss_update_toast(ws);
            } else if rect_contains(toast.geometry.details, x, y) {
                self.dismiss_update_toast(ws);
                self.open_about_panel(ws);
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ordinary_terminal_keys_and_modified_shortcuts_do_not_belong_to_toast() {
        for key in [Key::Character("a".into()), Key::Named(NamedKey::ArrowUp)] {
            assert!(!owns_key(true, &key, ModifiersState::empty()));
        }
        assert!(owns_key(
            true,
            &Key::Named(NamedKey::Tab),
            ModifiersState::SHIFT
        ));
        assert!(!owns_key(
            false,
            &Key::Named(NamedKey::Enter),
            ModifiersState::empty()
        ));
        for mods in [
            ModifiersState::CONTROL,
            ModifiersState::ALT,
            ModifiersState::SUPER,
        ] {
            assert!(!owns_key(true, &Key::Named(NamedKey::Enter), mods));
        }
    }

    #[test]
    fn held_toast_activation_cannot_repeat_into_pending_terminal_input() {
        use winit::keyboard::KeyCode;
        for key in [NamedKey::Escape, NamedKey::Enter, NamedKey::Space] {
            let mut closing = HashSet::new();
            let physical = PhysicalKey::Code(KeyCode::Enter);
            guard_activation(&Key::Named(key), physical, &mut closing);
            assert!(drop_closing_key_repeat(
                &mut closing,
                physical,
                ElementState::Pressed,
                true
            ));
            assert!(!drop_closing_key_repeat(
                &mut closing,
                physical,
                ElementState::Released,
                false
            ));
            assert!(!drop_closing_key_repeat(
                &mut closing,
                physical,
                ElementState::Pressed,
                false
            ));
        }
        let mut closing = HashSet::new();
        guard_activation(
            &Key::Named(NamedKey::Tab),
            PhysicalKey::Code(KeyCode::Tab),
            &mut closing,
        );
        assert!(closing.is_empty(), "Tab may keep navigating the toast");
    }
}

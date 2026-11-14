//! Program notifications (OSC 9, OSC 777, `kettle.notify`) in the window:
//! routing, the toast's projection and lifetime, its keys, pointer and
//! accessibility. It shares the update toast's painter and lifetime and,
//! like it, never changes terminal geometry or takes a key the terminal
//! needs: Escape dismisses the toast on screen and still reaches the program.

use super::*;
use crate::notification_router::{
    Admission, Delivery, MAX_SOURCE_CHARS, Message, Route, Source, route, sanitize_text,
};
use crate::notification_toast::{NotificationToast, toast_accessibility, toast_text};

pub(super) const ACCESSIBILITY_NOTIFICATION_STATUS_ID: NodeId = NodeId((1 << 59) + 6);
pub(super) const ACCESSIBILITY_NOTIFICATION_SHOW_ID: NodeId = NodeId((1 << 59) + 7);
pub(super) const ACCESSIBILITY_NOTIFICATION_DISMISS_ID: NodeId = NodeId((1 << 59) + 8);

/// The notification toast as one frame shows it, with what its controls and
/// screen-reader nodes need.
pub(super) struct NotificationToastView {
    pub overlay: kettle_render::UpdateToastOverlay,
    pub source: Source,
    /// The toast offers Show: its pane exists and is not the one in front.
    pub show: bool,
    pub label: String,
    pub description: Option<String>,
    pub show_name: String,
}

/// A plain Escape press over a visible notification toast dismisses it.
/// The press is not consumed: the program in the pane gets it too, so Kettle
/// never takes Escape from vim or a shell.
pub(super) fn escape_dismisses_notification(
    state: ElementState,
    repeat: bool,
    key: &Key,
    mods: ModifiersState,
) -> bool {
    state == ElementState::Pressed
        && !repeat
        && matches!(key, Key::Named(NamedKey::Escape))
        && !mods.control_key()
        && !mods.alt_key()
        && !mods.super_key()
}

/// The window a notification is shown in: the one being dispatched, or
/// another in the map.
fn notification_window<'a>(
    windows: &'a mut std::collections::BTreeMap<u64, WindowState>,
    current: Option<&'a mut WindowState>,
    seq: u64,
) -> Option<&'a mut WindowState> {
    match current {
        Some(ws) if ws.seq == seq => Some(ws),
        _ => windows.get_mut(&seq),
    }
}

fn queue_notification_toast(
    windows: &mut std::collections::BTreeMap<u64, WindowState>,
    current: Option<&mut WindowState>,
    window: u64,
    delivery: Delivery,
) {
    let Some(ws) = notification_window(windows, current, window) else {
        return;
    };
    ws.notification_toasts
        .push(NotificationToast::new(delivery, std::time::Instant::now()));
    ws.accessibility_pending = true;
    if let Some(native) = &ws.window {
        native.request_redraw();
    }
}

/// The source tab's title, sanitized like the notification's own text.
fn notification_label(
    ws: &WindowState,
    pane: Option<u64>,
    tr: &kettle_i18n::Translator,
) -> Option<String> {
    let index = ws.mux.tab_index_of_any_pane(&[pane?])?;
    let label = ws.mux.tab_labels(tr).into_iter().nth(index)?;
    Some(sanitize_text(&label.text, false, MAX_SOURCE_CHARS)).filter(|label| !label.is_empty())
}

pub(super) fn notification_toast_nodes(
    view: &NotificationToastView,
    tr: &kettle_i18n::Translator,
) -> Vec<(NodeId, Node)> {
    let mut nodes = Vec::with_capacity(3);
    let mut status = Node::new(Role::Status);
    status.set_label(view.label.clone());
    if let Some(description) = &view.description {
        status.set_description(description.clone());
    }
    status.set_live(accesskit::Live::Polite);
    let rect = view.overlay.geometry.rect;
    status.set_bounds(accesskit::Rect::new(
        f64::from(rect.0),
        f64::from(rect.1),
        f64::from(rect.0 + rect.2),
        f64::from(rect.1 + rect.3),
    ));
    nodes.push((ACCESSIBILITY_NOTIFICATION_STATUS_ID, status));
    let geometry = view.overlay.control_geometry();
    let dismiss = tr.text(kettle_i18n::Text::NoticeDismiss).to_string();
    for (id, label, rect) in [
        (
            ACCESSIBILITY_NOTIFICATION_SHOW_ID,
            view.show.then(|| view.show_name.clone()),
            geometry.details,
        ),
        (
            ACCESSIBILITY_NOTIFICATION_DISMISS_ID,
            Some(dismiss),
            geometry.close,
        ),
    ] {
        let Some(label) = label else {
            continue;
        };
        let mut node = Node::new(Role::Button);
        node.set_label(label);
        node.add_action(AccessibilityAction::Click);
        node.add_action(AccessibilityAction::Focus);
        node.set_bounds(accesskit::Rect::new(
            f64::from(rect.0),
            f64::from(rect.1),
            f64::from(rect.0 + rect.2),
            f64::from(rect.1 + rect.3),
        ));
        nodes.push((id, node));
    }
    nodes
}

fn rects_overlap(a: kettle_render::Rect4, b: kettle_render::Rect4) -> bool {
    a.0 < b.0 + b.2 && b.0 < a.0 + a.2 && a.1 < b.1 + b.3 && b.1 < a.1 + a.3
}

impl App {
    /// A program asked to notify: OSC 9 or 777 from `pane`, or a Lua
    /// script's `kettle.notify` (no pane) in this window. The router's
    /// cooldown and repeat rules apply before it picks the toast or the
    /// desktop.
    pub(super) fn notify_from_program(
        &mut self,
        ws: &mut WindowState,
        pane: Option<u64>,
        title: &str,
        body: &str,
    ) {
        let Some(message) = Message::new(title, body) else {
            return;
        };
        let source = Source {
            window: ws.seq,
            pane,
        };
        match self
            .notification_router
            .admit(source, message, std::time::Instant::now())
        {
            Admission::Deliver(delivery) => self.deliver_notification(Some(ws), delivery),
            Admission::Repeat { source, digest } => {
                for window in std::iter::once(&mut *ws).chain(self.windows.values_mut()) {
                    if window.notification_toasts.repeat(source, digest) {
                        window.accessibility_pending = true;
                        if let Some(native) = &window.window {
                            native.request_redraw();
                        }
                        break;
                    }
                }
            }
            // `about_to_wait` wakes for it when the source's cooldown ends.
            Admission::Held => {}
        }
    }

    /// Show held notifications whose source's cooldown has ended.
    pub(super) fn deliver_due_notifications(&mut self, now: std::time::Instant) {
        for delivery in self.notification_router.take_due(now) {
            self.deliver_notification(None, delivery);
        }
    }

    /// While a Kettle window has focus the notification is a toast there,
    /// and never also a desktop one; with none focused it goes to the desktop,
    /// remembered by id so a failure brings it back to its own window on its
    /// next focus.
    fn deliver_notification(
        &mut self,
        mut current: Option<&mut WindowState>,
        mut delivery: Delivery,
    ) {
        let origin = delivery.source.window;
        delivery.label = match current.as_deref() {
            Some(ws) if ws.seq == origin => Some(ws),
            _ => self.windows.get(&origin),
        }
        .and_then(|ws| notification_label(ws, delivery.source.pane, &self.ui_text));
        let focused = current
            .as_deref()
            .filter(|ws| ws.window_focused)
            .map(|ws| ws.seq)
            .or_else(|| {
                self.windows
                    .values()
                    .find(|ws| ws.window_focused)
                    .map(|ws| ws.seq)
            });
        match route(focused) {
            Route::Toast(window) => {
                queue_notification_toast(&mut self.windows, current, window, delivery);
            }
            Route::Desktop => {
                let now = std::time::Instant::now();
                let (title, body) = (
                    delivery.message.title.clone(),
                    delivery.message.body.clone(),
                );
                let id = self.notification_pending.insert(delivery, now);
                if !crate::queue_tracked_desktop_notification(id, &title, &body)
                    && let Some(refused) = self.notification_pending.take(id, now)
                {
                    queue_notification_toast(
                        &mut self.windows,
                        current.as_deref_mut(),
                        origin,
                        refused,
                    );
                }
                if let Some(ws) = notification_window(&mut self.windows, current, origin)
                    && let Some(native) = &ws.window
                {
                    native.request_user_attention(Some(UserAttentionType::Informational));
                    ws.attention_active = true;
                }
            }
        }
    }

    /// Desktop notifications the platform refused or failed on come back to
    /// their own window, shown there on its next focus.
    pub(super) fn receive_failed_desktop_notifications(&mut self) {
        let now = std::time::Instant::now();
        for id in crate::take_failed_desktop_notifications() {
            if let Some(failed) = self.notification_pending.take(id, now) {
                let window = failed.source.window;
                queue_notification_toast(&mut self.windows, None, window, failed);
            }
        }
    }

    /// A closed window's held, pending and queued notifications go with it.
    pub(super) fn forget_window_notifications(&mut self, window: u64) {
        self.notification_router.forget_window(window);
        self.notification_pending.forget_window(window);
    }

    /// Whether `ws` may show its notification toast now: the window is in
    /// front and nothing modal covers the terminal. Time counts only then.
    fn notification_toast_shown(&self, ws: &WindowState) -> bool {
        !ws.notification_toasts.is_empty()
            && ws.window_focused
            && ws.window_shown
            && !ws.window_occluded
            && ws
                .window
                .as_ref()
                .is_some_and(|w| w.is_minimized() != Some(true))
            && terminal_surface_available(ws)
            && !ws.search.open
            && !self.non_search_modal_open(ws)
    }

    /// The pane a toast came from exists and is not the one in front, so
    /// Show has somewhere to go.
    fn notification_origin_elsewhere(&self, ws: &WindowState, source: Source) -> bool {
        let Some(pane) = source.pane else {
            return false;
        };
        if source.window == ws.seq {
            ws.mux.panes.contains_key(&pane) && ws.mux.active_focus() != Some(pane)
        } else {
            self.windows
                .get(&source.window)
                .is_some_and(|window| window.mux.panes.contains_key(&pane))
        }
    }

    /// Top centre of the terminal area, below any persistent notice it
    /// would cover.
    fn notification_toast_geometry(
        &self,
        ws: &WindowState,
        message: &str,
        show: bool,
    ) -> Option<kettle_render::UpdateToastGeometry> {
        let renderer = ws.renderer.as_ref()?;
        let (width, height) = renderer.surface_size();
        let scale = ws.window.as_ref().map_or(1.0, |w| w.scale_factor() as f32);
        let cell = (
            renderer.overlay_text_cell_width(),
            renderer.overlay_text_line_height(),
        );
        let details = if show {
            self.ui_text
                .text(kettle_i18n::Text::NotificationToastShow)
                .width()
        } else {
            0
        };
        let notices = self.persistent_notices(ws);
        let mut top = self.area(ws).1;
        for _ in 0..=notices.len() {
            let geometry = kettle_render::update_toast_geometry(
                (width as f32, height as f32),
                top,
                scale,
                cell,
                details,
                message,
            )?;
            let below = notices
                .iter()
                .map(|(_, notice)| notice.geometry.rect)
                .filter(|rect| rects_overlap(*rect, geometry.rect))
                .map(|rect| rect.1 + rect.3)
                .fold(None, |lowest: Option<f32>, bottom| {
                    Some(lowest.map_or(bottom, |lowest| lowest.max(bottom)))
                });
            match below {
                Some(bottom) if bottom > top => top = bottom,
                _ => return Some(geometry),
            }
        }
        None
    }

    pub(super) fn notification_toast_projection(
        &self,
        ws: &WindowState,
    ) -> Option<NotificationToastView> {
        if !self.notification_toast_shown(ws) {
            return None;
        }
        let toast = ws.notification_toasts.visible()?;
        let renderer = ws.renderer.as_ref()?;
        let scale = ws.window.as_ref().map_or(1.0, |w| w.scale_factor() as f32);
        let columns = kettle_render::update_toast_columns(
            renderer.surface_size().0 as f32,
            scale,
            renderer.overlay_text_cell_width(),
        );
        let message = toast_text(toast, &self.ui_text, columns);
        let show = self.notification_origin_elsewhere(ws, toast.source);
        let geometry = self.notification_toast_geometry(ws, &message, show)?;
        let (label, description) = toast_accessibility(toast, &self.ui_text);
        let show_name = match toast.label.as_deref() {
            Some(source) => self.ui_text.notification_toast_show_a11y(source),
            None => self
                .ui_text
                .text(kettle_i18n::Text::NotificationToastShow)
                .to_string(),
        };
        Some(NotificationToastView {
            overlay: kettle_render::UpdateToastOverlay {
                geometry,
                message,
                details: if show {
                    self.ui_text
                        .text(kettle_i18n::Text::NotificationToastShow)
                        .into()
                } else {
                    String::new()
                },
                close: "\u{00d7}".into(),
                opacity: toast.clock.opacity(self.motion_allowed()),
                focused: match ws.notification_toast_focus {
                    Some(ACCESSIBILITY_NOTIFICATION_SHOW_ID) if show => Some(1),
                    Some(ACCESSIBILITY_NOTIFICATION_DISMISS_ID) => Some(2),
                    _ => None,
                },
            },
            source: toast.source,
            show,
            label,
            description,
            show_name,
        })
    }

    pub(super) fn dismiss_notification_toast(&mut self, ws: &mut WindowState) -> bool {
        if !ws.notification_toasts.dismiss() {
            return false;
        }
        ws.notification_toast_focus = None;
        self.sync_cursor_icon(ws);
        ws.accessibility_pending = true;
        if let Some(window) = &ws.window {
            window.request_redraw();
        }
        true
    }

    /// Show: go to the tab, pane and window the notification came from.
    pub(super) fn show_notification_origin(&mut self, ws: &mut WindowState, source: Source) {
        let Some(pane) = source.pane else {
            return;
        };
        if source.window == ws.seq {
            let pre = self.focus_key(ws);
            if ws.mux.focus_pane(pane) {
                ws.mux.touch_active_tab_seen();
                self.note_focus_change(ws, pre);
            }
        } else if let Some(mut target) = self.windows.remove(&source.window) {
            let pre = self.focus_key(&target);
            if target.mux.focus_pane(pane) {
                target.mux.touch_active_tab_seen();
                self.note_focus_change(&mut target, pre);
            }
            self.reveal_window(&mut target);
            self.windows.insert(source.window, target);
        }
    }

    /// Activate the toast as a Show or Dismiss press would.
    fn activate_notification_toast(&mut self, ws: &mut WindowState, show: bool) {
        let Some(view) = self.notification_toast_projection(ws) else {
            return;
        };
        self.dismiss_notification_toast(ws);
        if show && view.show {
            self.show_notification_origin(ws, view.source);
        }
    }

    pub(super) fn press_notification_toast(
        &mut self,
        ws: &mut WindowState,
        x: f32,
        y: f32,
        button: u8,
    ) -> bool {
        let Some(view) = self.notification_toast_projection(ws) else {
            return false;
        };
        let Some(hit) = view.overlay.control_geometry().hit_test(x, y) else {
            if ws.notification_toast_focus.take().is_some() {
                ws.accessibility_pending = true;
                if let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
            return false;
        };
        if button == 0 {
            match hit {
                kettle_render::UpdateToastHit::Close => self.activate_notification_toast(ws, false),
                // The body does what Show does, or dismisses a toast that
                // has nowhere to go.
                kettle_render::UpdateToastHit::Details | kettle_render::UpdateToastHit::Body => {
                    self.activate_notification_toast(ws, true)
                }
            }
        }
        true
    }

    /// A key while assistive technology has the toast's Show or Dismiss
    /// focused (`update_toast::owns_key` decided it belongs to the toast).
    pub(super) fn notification_toast_key(&mut self, ws: &mut WindowState, key: &Key) {
        match key {
            Key::Named(NamedKey::Escape) => {
                self.dismiss_notification_toast(ws);
            }
            Key::Named(NamedKey::Tab) => {
                let show = self
                    .notification_toast_projection(ws)
                    .is_some_and(|view| view.show);
                ws.notification_toast_focus = Some(
                    if show
                        && ws.notification_toast_focus
                            == Some(ACCESSIBILITY_NOTIFICATION_DISMISS_ID)
                    {
                        ACCESSIBILITY_NOTIFICATION_SHOW_ID
                    } else {
                        ACCESSIBILITY_NOTIFICATION_DISMISS_ID
                    },
                );
                ws.accessibility_pending = true;
                if let Some(window) = &ws.window {
                    window.request_redraw();
                }
            }
            key if confirmation_space_key(key) || matches!(key, Key::Named(NamedKey::Enter)) => {
                let show = ws.notification_toast_focus == Some(ACCESSIBILITY_NOTIFICATION_SHOW_ID);
                self.activate_notification_toast(ws, show);
            }
            _ => {
                ws.notification_toast_focus = None;
                ws.accessibility_pending = true;
            }
        }
    }

    /// An assistive-technology action on the toast. `true` when it was one.
    pub(super) fn notification_toast_action(
        &mut self,
        ws: &mut WindowState,
        request: &ActionRequest,
    ) -> bool {
        if !matches!(
            request.target_node,
            ACCESSIBILITY_NOTIFICATION_STATUS_ID
                | ACCESSIBILITY_NOTIFICATION_SHOW_ID
                | ACCESSIBILITY_NOTIFICATION_DISMISS_ID
        ) {
            return false;
        }
        if self.notification_toast_projection(ws).is_some() {
            match request.action {
                AccessibilityAction::Focus
                    if request.target_node != ACCESSIBILITY_NOTIFICATION_STATUS_ID =>
                {
                    ws.notification_toast_focus = Some(request.target_node);
                    ws.accessibility_pending = true;
                    if let Some(window) = &ws.window {
                        window.request_redraw();
                    }
                }
                AccessibilityAction::Click => {
                    let show = request.target_node == ACCESSIBILITY_NOTIFICATION_SHOW_ID;
                    self.activate_notification_toast(ws, show);
                }
                _ => {}
            }
        }
        true
    }

    /// Count the visible toast's time and dismiss it when it is up; the
    /// next waiting one follows. Returns when to look again.
    pub(super) fn tick_notification_toast(
        &mut self,
        ws: &mut WindowState,
        now: std::time::Instant,
    ) -> Option<std::time::Duration> {
        if ws.notification_toasts.is_empty() {
            ws.notification_toast_focus = None;
            return None;
        }
        let view = self.notification_toast_projection(ws);
        if view.is_none() {
            ws.notification_toast_focus = None;
        }
        let hovered = view.as_ref().is_some_and(|view| {
            ws.native_pointer.is_some_and(|p| {
                view.overlay
                    .geometry
                    .hit_test(p.x as f32, p.y as f32)
                    .is_some()
            })
        });
        let paused = hovered || ws.notification_toast_focus.is_some();
        let motion = self.motion_allowed();
        let (expired, fading, wake) = {
            let toast = ws.notification_toasts.visible_mut()?;
            toast.clock.tick(now, view.is_some(), paused);
            let remaining = toast.clock.remaining();
            (
                remaining.is_zero(),
                motion && remaining <= std::time::Duration::from_millis(150),
                toast.clock.wake_after(motion),
            )
        };
        if expired {
            self.dismiss_notification_toast(ws);
            // The next toast starts counting once a frame shows it.
            return (!ws.notification_toasts.is_empty())
                .then_some(std::time::Duration::from_millis(16));
        }
        if view.is_some()
            && !paused
            && fading
            && let Some(window) = &ws.window
        {
            window.request_redraw();
        }
        wake
    }
}

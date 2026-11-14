/// The body of production function `name` (the App's source with
/// `pub(super) ` dropped).
fn production_fn(src: &str, name: &str) -> String {
    src.split(&format!("fn {name}("))
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .unwrap_or_else(|| panic!("fn {name}"))
        .to_string()
}

/// OSC 9/777 and `kettle.notify` reach the router with their text
/// sanitized, and the router's cooldown and repeat suppression run before
/// any route is picked, so neither a focused nor an unfocused flood reaches
/// the desktop queue.
#[test]
fn program_notifications_pass_the_router_before_any_route() {
    let src = production_source();
    let notify = production_fn(&src, "notify_from_program");
    let sanitize = notify.find("Message::new(title, body)").expect("sanitized");
    let admit = notify.find(".admit(source, message,").expect("admitted");
    let deliver = notify.find("self.deliver_notification(Some(ws), delivery)").unwrap();
    assert!(sanitize < admit && admit < deliver);
    assert!(!notify.contains("queue_tracked_desktop_notification"));
    assert!(!notify.contains("fire_notify"));
    let lua = src
        .split("PendingLuaCommand::Notify { title, body, .. } => {")
        .nth(1)
        .and_then(|rest| rest.split('}').next())
        .expect("Lua notify arm");
    assert!(lua.contains("self.notify_from_program(ws, None, &title, &body);"));
    assert!(!lua.contains("fire_notify"));
    // Held messages are delivered when their cooldown ends, and the loop
    // wakes for it.
    let wait = src
        .split("fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {")
        .nth(1)
        .unwrap();
    assert!(wait.contains("self.deliver_due_notifications(now);"));
    assert!(wait.contains("self.notification_router.next_due(now)"));
}

/// The routing truth table, wired: a focused Kettle window (the origin or
/// another) gets a toast and nothing goes to the desktop; with none focused
/// the desktop gets it, remembered by id; a notification the worker refuses
/// at once, or reports failed later, comes back as a toast in its own
/// window, which shows it on its next focus.
#[test]
fn program_notifications_route_to_the_focused_window_or_the_desktop() {
    let src = production_source();
    let deliver = production_fn(&src, "deliver_notification");
    assert!(deliver.contains(".filter(|ws| ws.window_focused)"));
    assert!(deliver.contains(".find(|ws| ws.window_focused)"));
    let (toast, desktop) = deliver
        .split_once("Route::Desktop => {")
        .expect("both routes");
    let toast = toast.split("Route::Toast(window) => {").nth(1).unwrap();
    assert!(toast.contains("queue_notification_toast(&mut self.windows, current, window, delivery);"));
    assert!(!toast.contains("queue_tracked_desktop_notification"));
    let insert = desktop
        .find("self.notification_pending.insert(delivery, now)")
        .unwrap();
    let send = desktop
        .find("crate::queue_tracked_desktop_notification(id, &title, &body)")
        .unwrap();
    assert!(insert < send, "remembered before it can fail");
    assert!(desktop.contains("self.notification_pending.take(id, now)"));
    assert!(desktop.contains("origin,\n                        refused,"));
    let failed = production_fn(&src, "receive_failed_desktop_notifications");
    assert!(failed.contains("crate::take_failed_desktop_notifications()"));
    assert!(failed.contains("self.notification_pending.take(id, now)"));
    assert!(failed.contains("let window = failed.source.window;"));
    // A queued toast waits for its window's focus: time counts only while
    // it is shown, and it is shown only in a focused window.
    let shown = production_fn(&src, "notification_toast_shown");
    assert!(shown.contains("&& ws.window_focused"));
    assert!(src.contains("UserEvent::DesktopNotificationFailed => self.receive_failed_desktop_notifications(),"));
    assert!(src.contains("crate::on_desktop_notification_failure(move || {"));
    // A closed window's pending and held notifications go with it.
    assert!(src.contains("self.forget_window_notifications(seq);"));
}

/// Plain Escape over a visible notification toast dismisses the toast on
/// screen and is not consumed, so the program still gets it. With
/// modifiers, held down or released it does nothing; ordinary keys never
/// touch the toast; assistive focus on a button makes Escape, Enter, Space
/// and Tab the toast's.
#[test]
fn escape_dismisses_the_visible_toast_and_still_reaches_the_program() {
    use super::escape_dismisses_notification;
    use winit::event::ElementState;
    use winit::keyboard::{Key, ModifiersState, NamedKey};
    let escape = Key::Named(NamedKey::Escape);
    assert!(escape_dismisses_notification(
        ElementState::Pressed,
        false,
        &escape,
        ModifiersState::empty()
    ));
    assert!(escape_dismisses_notification(
        ElementState::Pressed,
        false,
        &escape,
        ModifiersState::SHIFT
    ));
    for mods in [
        ModifiersState::CONTROL,
        ModifiersState::ALT,
        ModifiersState::SUPER,
    ] {
        assert!(!escape_dismisses_notification(
            ElementState::Pressed,
            false,
            &escape,
            mods
        ));
    }
    assert!(!escape_dismisses_notification(
        ElementState::Pressed,
        true,
        &escape,
        ModifiersState::empty()
    ));
    assert!(!escape_dismisses_notification(
        ElementState::Released,
        false,
        &escape,
        ModifiersState::empty()
    ));
    for key in [
        Key::Named(NamedKey::Enter),
        Key::Named(NamedKey::Tab),
        Key::Character("q".into()),
    ] {
        assert!(!escape_dismisses_notification(
            ElementState::Pressed,
            false,
            &key,
            ModifiersState::empty()
        ));
    }
    let src = production_source();
    let arm = src
        .split("WindowEvent::KeyboardInput { event, .. } => {")
        .nth(1)
        .and_then(|rest| rest.split("WindowEvent::").next())
        .expect("keyboard arm");
    let pass = arm
        .split("&& self.notification_toast_projection(ws).is_some()\n                {")
        .nth(1)
        .and_then(|rest| rest.split("\n                }\n").next())
        .expect("the Escape dismissal");
    assert_eq!(pass.trim(), "self.dismiss_notification_toast(ws);");
    let dismissal = arm.find("escape_dismisses_notification(").unwrap();
    let terminal = arm
        .rfind("self.write_terminal_key_event(ws, &event, true)")
        .or_else(|| arm.rfind("write_terminal_key_event"))
        .unwrap();
    assert!(dismissal < terminal, "the key goes on to the terminal");
    // The owned-key path is the update toast's rule: focus first.
    assert!(arm.contains("ws.notification_toast_focus.is_some()\n                        && self.notification_toast_projection(ws).is_some(),"));
    // Dismissing takes the visible toast, the one the user sees.
    let dismiss = production_fn(&src, "dismiss_notification_toast");
    assert!(dismiss.contains("ws.notification_toasts.dismiss()"));
}

fn notification_view(label: Option<&str>, show: bool) -> super::NotificationToastView {
    let tr = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    let geometry =
        kettle_render::update_toast_geometry((800.0, 600.0), 0.0, 1.0, (8.0, 16.0), 4, "Build finished")
            .unwrap();
    super::NotificationToastView {
        overlay: kettle_render::UpdateToastOverlay {
            geometry,
            message: "fixture: Build finished".into(),
            details: if show { "Show".into() } else { String::new() },
            close: "\u{00d7}".into(),
            opacity: 1.0,
            focused: None,
        },
        source: crate::notification_router::Source {
            window: 1,
            pane: Some(2),
        },
        show,
        label: String::new(),
        description: None,
        show_name: label.map_or_else(
            || tr.text(kettle_i18n::Text::NotificationToastShow).to_string(),
            |source| tr.notification_toast_show_a11y(source),
        ),
    }
}

/// The toast is a polite AccessKit status, so every platform announces it
/// (macOS announcement, UIA live region, AT-SPI announcement), labelled with
/// its source and text in the UI's language; the count of repeats is its
/// description, so a repeat changes no label. Its buttons are named: Show
/// with the source tab, or plainly when there is none, and Dismiss.
#[test]
fn the_notification_toast_is_a_named_polite_status_in_both_languages() {
    use crate::notification_router::{Delivery, Message, Source};
    use crate::notification_toast::{NotificationToast, toast_accessibility};
    use accesskit::Role;
    for (language, label, show, dismiss) in [
        (
            kettle_i18n::Language::En,
            "Notification from fixture: Build finished",
            "Show fixture",
            "Dismiss",
        ),
        (
            kettle_i18n::Language::Es,
            "Notificaci\u{f3}n de fixture: Build finished",
            "Mostrar fixture",
            "Descartar",
        ),
    ] {
        let tr = kettle_i18n::Translator::new(language);
        let mut toast = NotificationToast::new(
            Delivery {
                source: Source {
                    window: 1,
                    pane: Some(2),
                },
                message: Message::new("Build finished", "").unwrap(),
                digest: 7,
                skipped: 0,
                label: Some("fixture".into()),
            },
            std::time::Instant::now(),
        );
        toast.repeats = 2;
        let mut view = notification_view(Some("fixture"), true);
        (view.label, view.description) = toast_accessibility(&toast, &tr);
        view.show_name = tr.notification_toast_show_a11y("fixture");
        let nodes = super::notification_toast_nodes(&view, &tr);
        let node = |id| {
            nodes
                .iter()
                .find(|(node, _)| *node == id)
                .map(|(_, node)| node.clone())
                .unwrap_or_else(|| panic!("{id:?}"))
        };
        let status = node(super::ACCESSIBILITY_NOTIFICATION_STATUS_ID);
        assert_eq!(status.role(), Role::Status);
        assert_eq!(status.live(), Some(accesskit::Live::Polite));
        assert_eq!(status.label(), Some(label));
        assert!(status.description().is_some(), "the repeat count");
        let show_node = node(super::ACCESSIBILITY_NOTIFICATION_SHOW_ID);
        assert_eq!(show_node.role(), Role::Button);
        assert_eq!(show_node.label(), Some(show));
        assert_eq!(
            node(super::ACCESSIBILITY_NOTIFICATION_DISMISS_ID).label(),
            Some(dismiss)
        );
    }
    // No source tab: Show reads "Show"; no Show button, no Show node.
    let tr = kettle_i18n::Translator::new(kettle_i18n::Language::En);
    let nodes = super::notification_toast_nodes(&notification_view(None, true), &tr);
    assert!(nodes.iter().any(|(id, node)| *id
        == super::ACCESSIBILITY_NOTIFICATION_SHOW_ID
        && node.label() == Some("Show")));
    let nodes = super::notification_toast_nodes(&notification_view(None, false), &tr);
    assert!(
        nodes
            .iter()
            .all(|(id, _)| *id != super::ACCESSIBILITY_NOTIFICATION_SHOW_ID)
    );
    // The tree carries the nodes and its change hash covers them, and AT
    // focus on a button is honoured.
    let src = production_source();
    assert!(src.contains("let projected = notification_toast_nodes(&view, &self.ui_text);"));
    assert!(src.contains("view.label.hash(&mut hasher);"));
    assert!(src.contains("view.description.hash(&mut hasher);"));
    assert!(src.contains(".notification_toast_focus\n                .filter(|target| nodes.iter().any(|(id, _)| id == target)))"));
}

/// Reduced motion: the toast appears and leaves without a fade. The source
/// tab's title, the third way text enters, is sanitized like the
/// notification's own. The update toast waits while a notification toast is
/// up. Nothing in the notification paths logs a title, a body or a label.
#[test]
fn the_notification_toast_respects_motion_sanitizes_its_source_and_logs_no_text() {
    let src = production_source();
    let projection = production_fn(&src, "notification_toast_projection");
    assert!(projection.contains("opacity: toast.clock.opacity(self.motion_allowed()),"));
    assert!(projection.contains("toast_text(toast, &self.ui_text, columns)"));
    assert!(projection.contains("kettle_render::update_toast_columns("));
    let label = src
        .split("fn notification_label(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .unwrap();
    assert!(label.contains("sanitize_text(&label.text, false, MAX_SOURCE_CHARS)"));
    let visible = production_fn(&src, "toast_visible");
    assert!(visible.contains("&& ws.notification_toasts.is_empty()"));
    for (name, source) in [
        ("app/notification_toast.rs", include_str!("notification_toast.rs")),
        (
            "notification_router.rs",
            include_str!("../notification_router.rs"),
        ),
        (
            "notification_toast.rs",
            include_str!("../notification_toast.rs"),
        ),
    ] {
        let production = kettle_test_support::production_source(source);
        for logger in ["log::", "println!", "eprintln!", "dbg!"] {
            assert!(
                !production.contains(logger),
                "{name}: notification text must never be logged ({logger})"
            );
        }
    }
    let worker = kettle_test_support::production_source(include_str!("../notifications.rs"));
    for line in worker.lines().filter(|line| line.contains("log::")) {
        for field in ["title", "body", ".message"] {
            assert!(!line.contains(field), "a log line names {field}: {line}");
        }
    }
}

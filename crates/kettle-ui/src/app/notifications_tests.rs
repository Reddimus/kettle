#[test]
fn input_rejections_have_distinct_rpc_semantics() {
    use kettle_ctl::protocol::error_codes as ec;

    assert_eq!(ctl_input_error(PaneInputResult::Queued), None);
    assert_eq!(
        ctl_input_error(PaneInputResult::ReadOnly).map(|error| error.0),
        Some(ec::READ_ONLY)
    );
    assert_eq!(
        ctl_input_error(PaneInputResult::Backpressured).map(|error| error.0),
        Some(ec::BUSY)
    );
    assert_eq!(
        ctl_input_error(PaneInputResult::Oversize).map(|error| error.0),
        Some(ec::BAD_PARAMS)
    );
    assert_eq!(
        ctl_input_error(PaneInputResult::Failed).map(|error| error.0),
        Some(ec::INTERNAL)
    );
}

#[test]
fn desktop_notifications_never_call_the_os_backend_on_the_ui_thread() {
    let src = production_source();
    let body = src
        .split("pub(crate) fn fire_notify(")
        .nth(1)
        .and_then(|rest| rest.split("\n}\n").next())
        .expect("fire_notify body");
    assert!(body.contains("crate::queue_desktop_notification("));
    assert!(
        !src.contains("notify_rust::"),
        "the UI event-loop module must not be able to call a platform notifier"
    );
}

#[test]
fn protocol_notifications_are_wired_to_ui_dispatch() {
    let src = production_source();
    assert!(
        src.contains("drain_protocol_notifications()"),
        "drain_events must consume OSC 9/777 notification requests"
    );
    assert!(
        src.contains("fire_notify(&ev.title, &ev.body);"),
        "protocol notifications must use the desktop notification helper"
    );
    assert!(
        src.contains("\"protocol_notification\""),
        "protocol notifications must be visible to control/event subscribers"
    );
}

/// The visual bell's time profile: instant on, quadratic ease-out, gone
/// at `BELL_FLASH_DURATION`. The renderer scales the configured lightness
/// step by this value, so its shape is the flash's perceived shape.
#[test]
fn bell_flash_ramp_is_instant_on_and_eases_out_within_the_duration() {
    use super::{BELL_FLASH_DURATION, bell_flash_ramp};
    use std::time::Duration;

    assert_eq!(BELL_FLASH_DURATION, Duration::from_millis(300));
    assert_eq!(
        bell_flash_ramp(Duration::ZERO),
        1.0,
        "the frame it rang is the peak"
    );
    assert!((bell_flash_ramp(Duration::from_millis(150)) - 0.25).abs() < 1e-6);
    assert!((bell_flash_ramp(Duration::from_millis(30)) - 0.81).abs() < 1e-6);
    assert_eq!(bell_flash_ramp(BELL_FLASH_DURATION), 0.0);
    assert_eq!(bell_flash_ramp(Duration::from_secs(5)), 0.0);
    let mut last = 1.0_f32;
    for ms in (0..=300).step_by(10) {
        let value = bell_flash_ramp(Duration::from_millis(ms));
        assert!(value <= last, "the ramp must never rise again ({ms} ms)");
        assert!((0.0..=1.0).contains(&value));
        last = value;
    }
    // Ease-out: the first half sheds most of the strength.
    assert!(bell_flash_ramp(Duration::from_millis(150)) < 0.5);
}

/// The bell flash is per pane: the drain stamps every ringing pane, the
/// frame builder turns each stamp into the pane's ramp, the idle loop
/// keeps the animation wake alive while any stamp is live and drops
/// expired stamps with one erasing repaint, and nothing keeps a
/// window-level bell timestamp any more.
#[test]
fn the_bell_flash_is_stamped_per_pane_and_pruned_on_expiry() {
    let src = production_source();
    let drain = src
        .split("if self.cfg.bell.visual() {")
        .nth(1)
        .and_then(|body| body.split("if self.cfg.bell.attention()").next())
        .expect("visual bell branch in drain_events");
    assert!(
        drain.contains("for id in &bell_panes {")
            && drain.contains("ws.bell_flashes.insert(*id, now);"),
        "every ringing pane must get its own flash stamp"
    );
    assert!(
        src.contains("bell_flash: *bell_flash,")
            && src.contains(".map_or(0.0, |rang| bell_flash_ramp(rang.elapsed())),"),
        "the frame builder must feed each pane its own ramp"
    );
    let idle = src
        .split("let bells_expired = ws")
        .nth(1)
        .and_then(|body| body.split("let bell_active =").next())
        .expect("bell expiry block in about_to_wait");
    assert!(
        idle.contains(".retain(|_, rang| rang.elapsed() < BELL_FLASH_DURATION);")
            && idle.contains("w.request_redraw();"),
        "expired stamps must be dropped and erased with one more repaint"
    );
    assert!(
        src.contains("let bell_active = !render_hidden && !ws.bell_flashes.is_empty();"),
        "the animation wake must key off live per-pane stamps"
    );
    assert!(
        !src.contains("last_bell"),
        "the window-level bell timestamp is gone; a flash belongs to the pane that rang"
    );
}

#[test]
fn the_pane_bell_indicator_is_wired_to_real_pane_state() {
    let src = production_source();
    let at = src.find("metas.push((").expect("frame builder");
    let push: String = src[at..].chars().take(900).collect();
    assert!(
        push.contains("p.bell,"),
        "the frame must carry each pane's OWN bell state; a literal here is \
             what made `icon_bell` undrawable"
    );
    // Bounded to the drain body, with the needle assembled at runtime, so
    // the guard cannot match its own text and keep passing after the
    // production call is deleted.
    let needle = ["ws.mux.clear_focused", "_pane_bell();"].concat();
    let drain = src
        .split("fn drain_events(&mut self, ws: &mut WindowState) {")
        .nth(1)
        .and_then(|b| b.split("\n    }\n").next())
        .expect("drain_events body");
    assert!(
        drain.contains(needle.as_str()),
        "the focused pane's bell must be cleared on the drain path, where \
             the current focus is always known — clearing it only on \
             focus-change events leaks the latch on every other route focus \
             can take (tab switch, reap promoting a sibling, ctl, restore)"
    );
}

#[test]
fn malformed_config_notification_is_edge_triggered() {
    use super::should_notify_malformed;
    let none: Vec<String> = vec![];
    let a = vec!["cursor-style = beem".to_string()];
    let b = vec!["cursor-style = beem".to_string(), "bell = loud".to_string()];
    // Nothing malformed → never notify.
    assert!(!should_notify_malformed(&none, &none));
    assert!(!should_notify_malformed(&none, &a));
    // First appearance of a problem → notify.
    assert!(should_notify_malformed(&a, &none));
    // Same problem set on the next reload (e.g. a settings-persistence
    // write) → stay quiet.
    assert!(!should_notify_malformed(&a, &a));
    // A newly-introduced/changed problem → notify again.
    assert!(should_notify_malformed(&b, &a));
    // Fixing everything → no notification (and resets the latch).
    assert!(!should_notify_malformed(&none, &b));
}


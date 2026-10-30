#[test]
fn software_recovery_title_notice_is_translated() {
    use kettle_i18n::{Language, Text, Translator};

    let src = production_source();
    let desired = src
        .split("fn desired_window_title(")
        .nth(1)
        .and_then(|rest| rest.split("\n    }\n").next())
        .unwrap();
    // rustfmt may split the call across lines; compare without whitespace.
    let compact: String = desired.split_whitespace().collect();
    assert!(compact.contains("self.ui_text.text(kettle_i18n::Text::WindowTitleSoftwareFallback)"));
    let en = Translator::new(Language::En);
    let es = Translator::new(Language::Es);
    let key = Text::ALL
        .iter()
        .copied()
        .find(|key| en.text(*key) == "software rendering (GPU unavailable)")
        .expect("software fallback notice is in the typed catalogue");
    assert_eq!(es.text(key), "renderizado por software (GPU no disponible)");
}

/// Source-drift guard: while the GPU device is lost the redraw guard must
/// reset the output pacer + snapshot `seen_output_gen` so a streaming pane
/// can't spin the loop. Recovery itself is driven from `about_to_wait_inner`
/// on a bounded backoff.
#[test]
fn gpu_lost_quiesces_and_schedules_recovery() {
    let src = production_source();
    let guard = src
        .find("if self.gpu.as_ref().is_some_and(|g| g.is_lost()) {")
        .expect("gpu_lost redraw guard present");
    let body = &src[guard..guard + 1400];
    assert!(
        body.contains("ws.output_pacer.reset()") && body.contains("ws.seen_output_gen.clear()"),
        "the gpu_lost redraw guard must reset pacing + snapshot seen_output_gen"
    );
    let wait = src
        .find("fn about_to_wait_inner(")
        .expect("about_to_wait_inner present");
    let wait_body = &src[wait..];
    // about_to_wait_inner is the last method in the impl, so the next
    // column-0 `}` closes the impl (excludes the test module below).
    let end = wait_body
        .find("\n}\n")
        .map(|i| i + 2)
        .unwrap_or(wait_body.len());
    assert!(
        wait_body[..end].contains("self.try_recover_gpu(ws,")
            && wait_body[..end].contains("gpu_recovery_backoff("),
        "about_to_wait_inner must drive GPU recovery while the device is lost"
    );
}

#[test]
fn live_gpu_context_uses_event_loop_owned_display_handle() {
    let src = production_source();
    let resumed = src
        .find("fn resumed_inner(")
        .expect("resumed_inner present");
    let resumed_tail = &src[resumed..];
    let resumed_body = &resumed_tail[..resumed_tail
        .find("\n    }\n")
        .expect("end of resumed_inner")];
    assert!(
        resumed_body.contains("Renderer::new_with_display_handle(")
            && resumed_body.contains("event_loop.owned_display_handle()"),
        "initial GPU creation must retain the event-loop display connection, not window 1"
    );

    let recovery = src
        .find("fn try_recover_gpu(")
        .expect("try_recover_gpu present");
    let recovery_tail = &src[recovery..];
    let recovery_end = recovery_tail
        .find("\n    }\n")
        .expect("try_recover_gpu end");
    let recovery_body = &recovery_tail[..recovery_end];
    assert!(
        recovery_body.contains("Renderer::new_with_escalation_and_display_handle(")
            && recovery_body.contains("event_loop.owned_display_handle()"),
        "GPU recovery must rebuild with the event-loop display connection"
    );
    assert!(
        recovery_body.contains("Self::stash_renderer_for_gpu_recovery(ws)")
            && recovery_body.contains("Self::stash_renderer_for_gpu_recovery(other)")
            && recovery_body.matches("restore_recovery_state(").count() == 2
            && recovery_body
                .matches("finish_gpu_renderer_recovery(")
                .count()
                == 2,
        "GPU recovery must retain, restore, and reflow every window around the atomic rebuild"
    );
}

/// The App owns each window's accent and may change it while the window
/// has no renderer (a config reload or a tear-off during GPU recovery), so
/// every renderer that replaces a window's renderer must be given it. A
/// renderer-held copy restored from before the change would draw the old
/// accent until the next config change.
#[test]
fn every_replacement_renderer_receives_the_window_accent() {
    let src = production_source();
    let body = |name: &str| {
        let start = src.find(name).unwrap_or_else(|| panic!("{name} present"));
        let tail = &src[start..];
        let end = tail[1..]
            .find("\n    }\n")
            .map_or(tail.len(), |end| end + 1);
        tail[..end].to_owned()
    };
    let finish = body("fn finish_gpu_renderer_recovery(");
    assert!(
        finish.contains("crate::window_accent::apply_accent(ws);"),
        "GPU recovery must apply each window's current accent claim"
    );
    let surface = body("fn try_recover_window_renderer(");
    let installed = surface
        .find("ws.renderer = Some(replacement);")
        .expect("surface recovery installs its replacement");
    assert!(
        surface[installed..].contains("crate::window_accent::apply_accent(ws);"),
        "surface recovery must apply the window's current accent claim"
    );
    // Every recovery path that installs a renderer finishes through one of
    // the two functions above.
    let recover = body("fn try_recover_gpu(");
    assert_eq!(
        recover.matches(".renderer = Some(").count(),
        2,
        "try_recover_gpu installs the primary and secondary renderers"
    );
    assert_eq!(recover.matches("finish_gpu_renderer_recovery(").count(), 2);
}

#[test]
fn gpu_recovery_backoff_grows_and_caps() {
    let ms = |attempt| super::gpu_recovery_backoff(attempt).as_millis();
    assert_eq!(ms(1), 500);
    assert_eq!(ms(2), 1000);
    assert_eq!(ms(3), 2000);
    assert_eq!(ms(4), 4000);
    assert_eq!(ms(50), 4000);
    for attempt in 1..20 {
        assert!(super::gpu_recovery_backoff(attempt) <= super::gpu_recovery_backoff(attempt + 1));
    }
}

#[test]
fn gpu_recovery_set_is_all_or_nothing_with_injected_factory() {
    let mut calls = Vec::new();
    let result = super::build_recovery_set([(2, "a"), (3, "b"), (4, "c")], |value| {
        calls.push(value);
        if value == "b" {
            Err("unsupported surface")
        } else {
            Ok(value.to_ascii_uppercase())
        }
    });

    assert_eq!(calls, ["a", "b", "c"]);
    assert_eq!(result, Err(vec![(3, "unsupported surface")]));

    let rebuilt = super::build_recovery_set([(2, 20), (3, 30)], Ok::<_, ()>).unwrap();
    assert_eq!(rebuilt, [(2, 20), (3, 30)]);
}

#[test]
fn gpu_recovery_snapshot_survives_failed_attempts_until_commit() {
    let mut retained = None;
    super::retain_renderer_recovery_snapshot(&mut retained, Some("live-font-zoom"));
    assert_eq!(retained, Some("live-font-zoom"));

    // Failed candidates cannot replace the state captured from the last
    // live renderer, even after that renderer has been dropped.
    super::retain_renderer_recovery_snapshot(&mut retained, None);
    assert_eq!(retained, Some("live-font-zoom"));
    super::retain_renderer_recovery_snapshot(&mut retained, Some("candidate-defaults"));
    assert_eq!(retained, Some("live-font-zoom"));

    // Successful commit is the only point that clears the retained state.
    retained = None;
    super::retain_renderer_recovery_snapshot(&mut retained, Some("post-recovery-live"));
    assert_eq!(retained, Some("post-recovery-live"));
}

/// A fresh window starts its first pane before it creates the window and
/// the GPU, so the shell starts while they initialize. The renderer then
/// takes over the fonts the pane was sized with.
#[test]
fn a_fresh_window_starts_its_first_pane_before_the_window_and_gpu() {
    let src = super::production_source();
    let body = src
        .split_once("fn resumed_inner(")
        .expect("resumed_inner")
        .1;
    let spawn = body
        .find("self.spawn_first_tab(ws, has_launch_override)")
        .expect("early spawn");
    let window = body
        .find("event_loop.create_window(attrs)")
        .expect("window creation");
    let gpu = body
        .find("Renderer::new_with_display_handle(")
        .expect("renderer init");
    assert!(spawn < window && window < gpu);
    assert!(body[gpu..].contains("ws.startup_fonts.take(),"));
}

/// Drift guard. The GPU device-picker branch writes THREE keys
/// (vendor/device/name) and must check every `persist_pref` result.
/// Setting `settings_restart_pending = true` when none of the writes landed
/// on disk tells the user a restart will apply a pin that silently reverts
/// to auto. A behavioral test needs a live Settings overlay + a failing
/// config write; pin the checked-result shape at the source.
#[test]
fn gpu_picker_checks_persist_results_before_flagging_restart() {
    let src = production_source();
    let arm = src
        .split("if key_str == \"gpu\" {")
        .nth(1)
        .and_then(|s| {
            s.split("\n        // Notify if the Settings change can't")
                .next()
        })
        .expect("gpu picker branch present");
    assert!(
        arm.contains("let a = self.persist_pref(\"gpu-vendor-id\", \"0\");")
            && arm.contains("a && b && c"),
        "the auto branch must fold all three persist_pref results together"
    );
    assert!(
        arm.contains("if saved {")
            && arm.contains("ws.settings_restart_pending = true;")
            && arm.contains("fire_notify("),
        "settings_restart_pending must only be set when the writes actually succeeded, \
             and a failure must be surfaced via fire_notify"
    );
}

#[test]
fn gpu_init_watchdog_fires_only_on_a_real_hang() {
    use super::gpu_init_timed_out;
    use std::sync::atomic::AtomicBool;
    use std::time::Duration;
    // Drift guard. `done` never set within the window → reports a
    // hang (true), so the real watchdog would log + exit.
    let stuck = AtomicBool::new(false);
    assert!(gpu_init_timed_out(
        &stuck,
        Duration::from_millis(60),
        Duration::from_millis(10)
    ));
    // `done` already set (init finished) → stands down immediately (false),
    // even with a long nominal timeout, so a fast init/failure is never
    // killed.
    let finished = AtomicBool::new(true);
    assert!(!gpu_init_timed_out(
        &finished,
        Duration::from_secs(30),
        Duration::from_millis(10)
    ));
}

#[test]
fn command_deadlines_are_maintained_before_gpu_recovery() {
    let src = production_source();
    let scheduler = src.split("fn about_to_wait_inner(").nth(1).unwrap();
    let maintenance = scheduler
        .find("self.check_pending_run_deadlines(ws);")
        .unwrap();
    let recovery = scheduler
        .find("if self.gpu.as_ref().is_some_and(|g| g.is_lost())")
        .unwrap();
    assert!(
        maintenance < recovery,
        "GPU recovery waits must not bypass command timeout maintenance"
    );
    assert!(scheduler[..recovery].contains("chain(pending_run_wait)"));
    let recovery = scheduler[recovery..]
        .split("if ws.pty_resize_retry")
        .next()
        .unwrap();
    assert_eq!(
        recovery.matches("automation_wait.map_or").count(),
        2,
        "both recovery backoff returns must retain the command deadline"
    );
}


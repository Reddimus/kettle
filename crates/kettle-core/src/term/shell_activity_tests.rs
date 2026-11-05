use super::{ShellActivity, classify_shell_activity};

#[test]
#[cfg(unix)]
fn resizing_a_multiline_prompt_preserves_shell_state_and_keyboard_claims() {
    use super::{CursorShape, PtyGeometry, Terminal};
    use std::sync::Arc;
    use std::time::{Duration, Instant};
    let (tx, _rx) = crossbeam_channel::unbounded();
    let mut terminal = Terminal::new(
        &["/bin/sh".into(), "-c".into(),
            r"printf '\033]133;A\007\033]133;C\007\033]133;D;0\007\033]133;A\007prompt\r\nREADY\033[>1u'; read answer".into()],
        None, 100, 80, 24, 8, 16, false, CursorShape::Block, None, tx, Arc::new(|| {}),
    ).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while !terminal
        .screen_text(0)
        .is_some_and(|screen| screen.text.contains("READY"))
    {
        assert!(Instant::now() < deadline, "prompt fixture did not arrive");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(terminal.shell_activity(), ShellActivity::Idle);
    assert!(terminal.keyboard_claims().at_prompt);
    assert!(terminal.keyboard_claims().keyboard_protocol);
    terminal
        .try_resize_geometry(PtyGeometry::new(100, 24, 800, 384))
        .unwrap();
    assert!(terminal.prompts.lock().unwrap().is_empty());
    assert_eq!(
        terminal.shell_activity(),
        ShellActivity::Idle,
        "discarding navigation anchors must preserve observed shell state"
    );
    assert!(terminal.keyboard_claims().at_prompt);
    assert!(terminal.keyboard_claims().keyboard_protocol);
}

#[test]
fn command_admission_rejects_running_shell() {
    let started = std::sync::Mutex::new(Some(std::time::Instant::now()));
    let generation = std::sync::atomic::AtomicU64::new(7);
    assert_eq!(
        super::command_admission_generation(&started, &generation),
        None
    );
    *started.lock().unwrap() = None;
    assert_eq!(
        super::command_admission_generation(&started, &generation),
        Some(7)
    );
}

#[test]
fn incomplete_integration_never_claims_idle_or_running() {
    for (prompts, tracking) in [(false, false), (true, false), (false, true)] {
        for running in [Some(false), Some(true), None] {
            assert_eq!(
                classify_shell_activity(prompts, tracking, running),
                ShellActivity::Unknown
            );
        }
    }
}

#[test]
fn complete_integration_distinguishes_prompt_and_command() {
    assert_eq!(
        classify_shell_activity(true, true, Some(false)),
        ShellActivity::Idle
    );
    assert_eq!(
        classify_shell_activity(true, true, Some(true)),
        ShellActivity::Running
    );
    assert_eq!(
        classify_shell_activity(true, true, None),
        ShellActivity::Unknown
    );
}

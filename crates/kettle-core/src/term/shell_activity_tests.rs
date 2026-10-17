use super::{ShellActivity, classify_shell_activity};

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

use super::{WorkingDirectoryPolicy, apply_working_directory};
use portable_pty::CommandBuilder;

/// Automation checks its directory before spawning, so the spawn must use
/// that directory or fail. Interactive panes keep their HOME recovery.
#[test]
fn only_automation_requires_its_directory_at_spawn() {
    let missing = std::path::Path::new("/kettle-no-such-directory");
    let mut automation = CommandBuilder::new("sh");
    apply_working_directory(
        &mut automation,
        Some(missing),
        WorkingDirectoryPolicy::RejectInvalidExplicit,
    );
    assert_eq!(
        automation.get_cwd().map(|dir| dir.as_os_str()),
        Some(missing.as_os_str())
    );
    assert!(automation.get_require_cwd());

    let mut pane = CommandBuilder::new("sh");
    apply_working_directory(
        &mut pane,
        Some(missing),
        WorkingDirectoryPolicy::FallbackToHome,
    );
    assert!(!pane.get_require_cwd());
    assert_ne!(
        pane.get_cwd().map(|dir| dir.as_os_str()),
        Some(missing.as_os_str())
    );
}

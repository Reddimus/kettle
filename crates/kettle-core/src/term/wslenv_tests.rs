use super::{augment_wslenv, child_wslenv};

#[test]
fn appends_with_u_flag_preserves_existing_and_dedups() {
    let vars = ["COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION"];
    // Empty existing → just our vars, each `/u`.
    assert_eq!(
        augment_wslenv("", &vars),
        "COLORTERM/u:TERM_PROGRAM/u:TERM_PROGRAM_VERSION/u"
    );
    // The user's existing WSLENV is preserved verbatim, ours appended.
    assert_eq!(
        augment_wslenv("FOO/p:BAR", &["COLORTERM"]),
        "FOO/p:BAR:COLORTERM/u"
    );
    // An entry the user already has (even with a different flag) is not
    // duplicated; matching is on the name before the `/flags`.
    assert_eq!(
        augment_wslenv("COLORTERM/up:X", &["COLORTERM", "TERM_PROGRAM"]),
        "COLORTERM/up:X:TERM_PROGRAM/u"
    );
    assert_eq!(augment_wslenv("COLORTERM", &["COLORTERM"]), "COLORTERM");
    assert_eq!(
        augment_wslenv("EDITOR/u", &["EDITOR", "KETTLE_ENV", "EDITOR"]),
        "EDITOR/u:KETTLE_ENV/u"
    );
}

#[test]
fn child_wslenv_preserves_user_base_and_forwards_extra_env() {
    let extra_env = vec![
        ("EDITOR".to_string(), "nvim".to_string()),
        ("WSLENV".to_string(), "USER_BASE/p".to_string()),
        ("EDITOR".to_string(), "vim".to_string()),
    ];
    assert_eq!(
        child_wslenv("PARENT/p", &extra_env),
        "USER_BASE/p:EDITOR/u:COLORTERM/u:TERM_PROGRAM/u:TERM_PROGRAM_VERSION/u"
    );
}

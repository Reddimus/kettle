//! `ask-before-closing` modes: the default asks about a busy single pane, and
//! every value a config file could already hold keeps its meaning.

use kettle_config::{AskBeforeClosing, Config};

fn parsed(value: &str) -> AskBeforeClosing {
    Config::parse_text(&format!("ask-before-closing = {value}")).ask_before_closing
}

#[test]
fn the_default_is_its_own_mode_not_multiple_terminals() {
    let default = Config::default().ask_before_closing;
    assert_ne!(default, AskBeforeClosing::MultipleTerminals);
    assert_eq!(default, parsed("busy-or-multiple"));
    assert_eq!(default, parsed("busy_or_multiple"));
    assert_eq!(default, parsed("BUSY-OR-MULTIPLE"));
    assert!(Config::detect_malformed_values("ask-before-closing = busy-or-multiple").is_empty());
    assert!(Config::detect_malformed_values("ask_before_closing = busy_or_multiple").is_empty());
    // An explicit `multiple-terminals` is a choice, so it no longer reads as
    // the default.
    assert_ne!(parsed("multiple-terminals"), default);
}

#[test]
fn values_written_before_the_new_mode_parse_as_they_did() {
    for (value, mode) in [
        ("always", AskBeforeClosing::Always),
        ("ALWAYS", AskBeforeClosing::Always),
        ("never", AskBeforeClosing::Never),
        ("multiple", AskBeforeClosing::MultipleTerminals),
        ("multiple-terminals", AskBeforeClosing::MultipleTerminals),
        ("multiple_terminals", AskBeforeClosing::MultipleTerminals),
    ] {
        assert_eq!(parsed(value), mode, "{value}");
        assert!(
            Config::detect_malformed_values(&format!("ask-before-closing = {value}")).is_empty(),
            "{value}"
        );
    }
    assert!(!Config::detect_malformed_values("ask-before-closing = alwyas").is_empty());
}

#[test]
fn never_never_prompts() {
    for scope in [0, 1, 2, 100] {
        assert!(!parsed("never").should_prompt(scope));
    }
}

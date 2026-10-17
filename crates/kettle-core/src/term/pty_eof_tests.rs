use super::{
    CanonicalEofRules, PTY_STDIN_EOF_TRACK_BYTES, PtyInputTail, WordEraseMode, pty_eof_sequence,
};

fn rules() -> CanonicalEofRules {
    CanonicalEofRules {
        canonical: true,
        extproc: false,
        igncr: false,
        icrnl: true,
        inlcr: false,
        istrip: false,
        iuclc: false,
        iexten: true,
        iutf8: true,
        isig: true,
        noflsh: false,
        ixon: true,
        veof: Some(0x04),
        veol: None,
        veol2: None,
        verase: Some(0x7f),
        vkill: Some(0x15),
        vlnext: Some(0x16),
        vwerase: Some(0x17),
        vreprint: Some(0x12),
        vdiscard: Some(0x0f),
        vintr: Some(0x03),
        vquit: Some(0x1c),
        vsusp: Some(0x1a),
        vstart: Some(0x11),
        vstop: Some(0x13),
        word_erase: WordEraseMode::Linux,
    }
}

fn unterminated(bytes: &[u8], rules: CanonicalEofRules) -> std::result::Result<bool, &'static str> {
    let mut input = PtyInputTail::default();
    input.observe(bytes, rules);
    input.record_unterminated(rules)
}

#[test]
fn unterminated_canonical_record_uses_configured_veof_twice() {
    assert_eq!(
        pty_eof_sequence(true, 0x1a, 0xff, true),
        Ok(Some(([0x1a; 2], 2)))
    );
}

#[test]
fn empty_or_terminated_canonical_input_uses_one_veof() {
    assert_eq!(
        pty_eof_sequence(true, 0x04, 0xff, false),
        Ok(Some(([0x04; 2], 1)))
    );
}

#[test]
fn noncanonical_input_never_injects_an_eof_character() {
    assert_eq!(pty_eof_sequence(false, 0x04, 0xff, true), Ok(None));
}

#[test]
fn disabled_canonical_veof_fails_explicitly() {
    assert_eq!(
        pty_eof_sequence(true, 0xff, 0xff, true),
        Err("the PTY has VEOF disabled")
    );
}

#[test]
fn live_cr_nl_mappings_select_the_canonical_boundary() {
    assert_eq!(unterminated(b"", rules()), Ok(false));
    assert_eq!(unterminated(b"x\n", rules()), Ok(false));
    assert_eq!(unterminated(b"x\r", rules()), Ok(false));
    assert_eq!(unterminated(b"x", rules()), Ok(true));

    let inlcr = CanonicalEofRules {
        inlcr: true,
        icrnl: false,
        ..rules()
    };
    assert_eq!(unterminated(b"x\n", inlcr), Ok(true));
    assert_eq!(
        unterminated(
            b"x\n",
            CanonicalEofRules {
                veol: Some(b'\r'),
                ..inlcr
            }
        ),
        Ok(false)
    );
}

#[test]
fn igncr_discards_cr_before_replaying_the_record() {
    let igncr = CanonicalEofRules {
        igncr: true,
        icrnl: true,
        ..rules()
    };
    assert_eq!(unterminated(b"\r\r", igncr), Ok(false));
    assert_eq!(unterminated(b"x\n\r\r", igncr), Ok(false));
    assert_eq!(unterminated(b"x\r\r", igncr), Ok(true));
}

#[test]
fn live_veof_veol_and_veol2_end_a_canonical_record() {
    assert_eq!(unterminated(b"x\x04", rules()), Ok(false));
    assert_eq!(
        unterminated(
            b"x;",
            CanonicalEofRules {
                veol: Some(b';'),
                ..rules()
            }
        ),
        Ok(false)
    );
    assert_eq!(
        unterminated(
            b"x:",
            CanonicalEofRules {
                veol2: Some(b':'),
                ..rules()
            }
        ),
        Ok(false)
    );
}

#[test]
fn vlnext_quotes_delimiters_and_i_exten_controls_it() {
    assert_eq!(unterminated(b"abc\x16\n", rules()), Ok(true));
    assert_eq!(
        unterminated(
            b"abc\x16\n",
            CanonicalEofRules {
                iexten: false,
                ..rules()
            }
        ),
        Ok(false)
    );
    assert_eq!(
        unterminated(b"abc\x16", rules()),
        Err("forwarded stdin ended after the canonical VLNEXT character"),
        "injecting VEOF into a pending literal-next state would corrupt input"
    );
}

#[test]
fn erase_kill_and_word_erase_update_the_pending_record() {
    assert_eq!(unterminated(b"x\x7f", rules()), Ok(false));
    assert_eq!(unterminated(b"abc\x15", rules()), Ok(false));
    assert_eq!(
        unterminated(b"abc def\x17", rules()),
        Ok(true),
        "word erase leaves the preceding record"
    );
    assert_eq!(unterminated(b"abc\x17", rules()), Ok(false));
    assert_eq!(
        unterminated("é\u{7f}".as_bytes(), rules()),
        Ok(false),
        "IUTF8 VERASE removes one complete UTF-8 character"
    );
    assert_eq!(
        unterminated(
            "é\u{7f}".as_bytes(),
            CanonicalEofRules {
                iutf8: false,
                ..rules()
            }
        ),
        Ok(true),
        "without IUTF8, VERASE removes only the trailing byte"
    );
}

#[test]
fn word_erase_follows_linux_and_bsd_line_disciplines() {
    assert_eq!(
        unterminated(b"abc-def\x17", rules()),
        Ok(true),
        "Linux stops after the trailing alphanumeric run at punctuation"
    );
    assert_eq!(
        unterminated(b"abc---\x17", rules()),
        Ok(false),
        "Linux consumes trailing punctuation before the preceding word"
    );

    let bsd_simple = CanonicalEofRules {
        word_erase: WordEraseMode::BsdSimple,
        ..rules()
    };
    assert_eq!(
        unterminated(b"abc-def\x17", bsd_simple),
        Ok(false),
        "BSD without ALTWERASE consumes through non-space input"
    );

    let bsd_alt = CanonicalEofRules {
        word_erase: WordEraseMode::BsdAlt,
        ..rules()
    };
    assert_eq!(
        unterminated(b"abc-def\x17", bsd_alt),
        Ok(true),
        "BSD ALTWERASE stops when the ASCII word class changes"
    );
    assert_eq!(
        unterminated(b"abc---\x17", bsd_alt),
        Ok(true),
        "BSD ALTWERASE erases only the trailing punctuation class"
    );
}

#[test]
fn extproc_never_uses_canonical_eof_injection() {
    let extproc = CanonicalEofRules {
        extproc: true,
        ..rules()
    };
    assert!(!extproc.supports_eof());

    let mut input = PtyInputTail::default();
    input.observe(b"untracked", extproc);
    assert_eq!(
        input.record_unterminated(rules()),
        Err("the child changed canonical termios while stdin had a pending record"),
        "turning EXTPROC off cannot reinterpret externally processed bytes"
    );
    input.observe(b"\n", rules());
    assert_eq!(
        input.record_unterminated(rules()),
        Ok(false),
        "a later canonical delimiter establishes a new safe boundary"
    );
}

#[test]
fn istrip_and_iuclc_apply_before_control_character_matching() {
    assert_eq!(
        unterminated(
            &[b'x', 0x84],
            CanonicalEofRules {
                istrip: true,
                ..rules()
            }
        ),
        Ok(false),
        "0x84 strips to VEOF"
    );
    assert_eq!(
        unterminated(
            b"xQ",
            CanonicalEofRules {
                iuclc: true,
                veol: Some(b'q'),
                ..rules()
            }
        ),
        Ok(false),
        "IUCLC maps Q to the q VEOL"
    );
}

#[test]
fn isig_and_ixon_controls_do_not_create_pending_input() {
    assert_eq!(unterminated(b"x\x03", rules()), Ok(false));
    assert_eq!(
        unterminated(
            b"x\x03",
            CanonicalEofRules {
                noflsh: true,
                ..rules()
            }
        ),
        Ok(true),
        "NOFLSH preserves the record while VINTR itself is consumed"
    );
    assert_eq!(unterminated(b"\x13", rules()), Ok(false));
    assert_eq!(unterminated(b"x\x13", rules()), Ok(true));
}

#[test]
fn oversized_record_is_nonempty_without_unbounded_retention() {
    let mut input = PtyInputTail::default();
    input.observe(&vec![b'x'; PTY_STDIN_EOF_TRACK_BYTES + 1], rules());
    assert_eq!(
        input.record_unterminated(rules()),
        Ok(true),
        "an oversized unterminated record conservatively requires two VEOF characters"
    );

    let mut line_delimited = vec![b'x'; PTY_STDIN_EOF_TRACK_BYTES + 1];
    line_delimited.push(b'\n');
    input = PtyInputTail::default();
    input.observe(&line_delimited, rules());
    input.observe(&line_delimited, rules());
    assert_eq!(
        input.record_unterminated(rules()),
        Ok(false),
        "completed records release overflow state and retained memory"
    );
}

#[test]
fn relevant_termios_change_with_pending_input_fails_closed() {
    let mut input = PtyInputTail::default();
    input.observe(b"x", rules());
    let changed = CanonicalEofRules {
        verase: Some(b'~'),
        ..rules()
    };
    assert_eq!(
        input.record_unterminated(changed),
        Err("the child changed canonical termios while stdin had a pending record")
    );
    input.observe(b"\n", changed);
    assert_eq!(
        input.record_unterminated(changed),
        Ok(false),
        "a stable record boundary clears earlier termios ambiguity"
    );
    input.observe(b"new-record", changed);
    assert_eq!(
        input.record_unterminated(changed),
        Ok(true),
        "tracking resumes under the stable rules after the boundary"
    );

    let mut terminated = PtyInputTail::default();
    terminated.observe(b"x\n", rules());
    assert_eq!(terminated.record_unterminated(changed), Ok(false));
}

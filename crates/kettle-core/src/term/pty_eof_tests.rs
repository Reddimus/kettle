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

#[cfg(unix)]
fn native_stdin_pair() -> (super::PtyStdin, std::fs::File) {
    use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};
    use std::sync::{Arc, Mutex};
    let (mut master, mut slave) = (-1, -1);
    assert_eq!(
        // SAFETY: master and slave are writable descriptor outputs; null name, termios and
        // winsize request defaults.
        unsafe {
            libc::openpty(
                &mut master,
                &mut slave,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        },
        0
    );
    // SAFETY: Successful openpty returned a new master descriptor with no other Rust owner.
    let master = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(master) });
    // SAFETY: Successful openpty returned a distinct new slave descriptor with no other Rust owner.
    let slave = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(slave) });
    // SAFETY: termios consists of integer fields and arrays, for which zero bits are valid.
    let mut attrs = unsafe { std::mem::zeroed::<libc::termios>() };
    // SAFETY: slave owns the live terminal descriptor and attrs is writable termios storage.
    assert_eq!(unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut attrs) }, 0);
    attrs.c_lflag |= libc::ICANON;
    attrs.c_lflag &= !libc::ECHO;
    attrs.c_cc[libc::VEOF] = 4;
    assert_eq!(
        // SAFETY: slave owns the live descriptor and attrs remains initialized from tcgetattr
        // with valid flag/byte edits.
        unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &attrs) },
        0
    );
    let lease = super::UnixPtyStdinLease::acquire(
        master.as_raw_fd(),
        Arc::new(Mutex::new(super::PtyStdinLeasePhase::Available)),
    )
    .unwrap();
    (
        super::PtyStdin {
            writer: super::PtyWriter(Arc::new(Mutex::new(Box::new(master)))),
            lease,
            input_state: super::PtyInputTail::default(),
            pending_eof: None,
        },
        slave,
    )
}

#[cfg(unix)]
fn read_native_record(slave: &mut std::fs::File) -> Vec<u8> {
    use std::io::Read as _;
    use std::os::fd::AsRawFd as _;
    let mut poll_fd = libc::pollfd {
        fd: slave.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert!(
        // SAFETY: poll_fd is writable storage for one pollfd; slave keeps the descriptor live.
        unsafe { libc::poll(&mut poll_fd, 1, 1000) } > 0,
        "canonical read did not become ready"
    );
    let mut bytes = [0; 32];
    let count = slave.read(&mut bytes).unwrap();
    bytes[..count].to_vec()
}

#[cfg(unix)]
#[test]
fn reply_between_eof_steps_requires_an_additional_empty_record_signal() {
    use super::PtyEofProgress;
    let (mut stdin, mut slave) = native_stdin_pair();
    assert_eq!(stdin.try_write(b"input").unwrap(), 5);
    assert_eq!(stdin.try_signal_eof().unwrap(), PtyEofProgress::Pending);
    assert_eq!(read_native_record(&mut slave), b"input");
    assert_eq!(stdin.try_write(b"reply").unwrap(), 5);
    assert_eq!(stdin.try_signal_eof().unwrap(), PtyEofProgress::Pending);
    assert_eq!(read_native_record(&mut slave), b"reply");
    assert_eq!(stdin.try_signal_eof().unwrap(), PtyEofProgress::Signaled);
    assert!(read_native_record(&mut slave).is_empty());
}

#[test]
fn eof_admission_rejects_mapped_or_shadowed_control_bytes() {
    for unsafe_rules in [
        CanonicalEofRules {
            veof: Some(b'\r'),
            ..rules()
        },
        CanonicalEofRules {
            veof: Some(b'\r'),
            igncr: true,
            ..rules()
        },
        CanonicalEofRules {
            veof: Some(b'\n'),
            ..rules()
        },
        CanonicalEofRules {
            veof: Some(0x84),
            istrip: true,
            ..rules()
        },
        CanonicalEofRules {
            veof: Some(b'A'),
            iuclc: true,
            ..rules()
        },
        CanonicalEofRules {
            veof: Some(0x16),
            ..rules()
        },
        CanonicalEofRules {
            veof: Some(0x03),
            ..rules()
        },
        CanonicalEofRules {
            veof: Some(0x13),
            ..rules()
        },
        CanonicalEofRules {
            veof: Some(0x7f),
            ..rules()
        },
    ] {
        assert!(
            !unsafe_rules.supports_eof(),
            "unsafe VEOF admission: {unsafe_rules:?}"
        );
    }
    assert!(
        CanonicalEofRules {
            veof: Some(b'\r'),
            icrnl: false,
            ..rules()
        }
        .supports_eof()
    );
    assert!(rules().supports_eof());
}

#[cfg(unix)]
#[test]
fn mapped_carriage_return_veof_is_not_injected() {
    use std::os::fd::AsRawFd as _;
    let (mut stdin, slave) = native_stdin_pair();
    // SAFETY: termios consists of integer fields and arrays, for which zero bits are valid.
    let mut attrs = unsafe { std::mem::zeroed::<libc::termios>() };
    // SAFETY: slave owns the live terminal descriptor and attrs is writable termios storage.
    assert_eq!(unsafe { libc::tcgetattr(slave.as_raw_fd(), &mut attrs) }, 0);
    attrs.c_cc[libc::VEOF] = b'\r';
    attrs.c_iflag |= libc::ICRNL;
    attrs.c_iflag &= !libc::IGNCR;
    assert_eq!(
        // SAFETY: slave owns the live descriptor and attrs remains initialized from tcgetattr
        // with valid flag/byte edits.
        unsafe { libc::tcsetattr(slave.as_raw_fd(), libc::TCSANOW, &attrs) },
        0
    );
    assert_eq!(
        stdin.try_signal_eof().unwrap(),
        super::PtyEofProgress::Unsupported
    );
    let mut poll_fd = libc::pollfd {
        fd: slave.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    assert_eq!(
        // SAFETY: poll_fd is writable storage for one pollfd; slave keeps the descriptor live.
        unsafe { libc::poll(&mut poll_fd, 1, 0) },
        0,
        "unsupported EOF must not deliver a newline"
    );
}

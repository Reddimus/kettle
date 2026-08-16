//! A single terminal instance: PTY + `alacritty_terminal` grid + VT parser,
//! driven by a dedicated reader thread.

use std::collections::{HashMap, VecDeque};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::term::cell::Cell;
use alacritty_terminal::term::{
    Config as TermConfig, GraphicsEvent, GraphicsEventBatch, GraphicsScrollDirection,
};
use alacritty_terminal::vte::ansi::{
    Color as AnsiColor, CursorShape, Handler, NamedColor, Processor, SYNC_MARKER_CAPACITY,
};
use anyhow::{Context, Result};
use kettle_vt::kitty::{Delete as KittyDelete, DeleteTarget as KittyDeleteTarget, PlacementKey};
use kettle_vt::placeholder::{self, CellDiacritics, RawCell};
use kettle_vt::{
    Chunk, CompletionList, CompletionUpdate, DeferredGraphics, Extractor, Progress, PromptKind,
};
use portable_pty::{CommandBuilder, PtySize};

use crate::event::{EventProxy, OutputWakeGate, TermEvent, Waker};
use crate::images::{
    AnimEntry, Animations, ImageSourceCrop, ImageSourceRect, Images, Placement, PlacementParams,
    RelEntry, Relatives, VirtualEntry, Virtuals, prune, relative_origin, resolve_chain,
};
use crate::persistence::{AsyncFileWriter, AsyncWriterStatus};

const PTY_READ_BUFFER_BYTES: usize = 64 * 1024;
const PTY_PUMP_QUEUE_DEPTH: usize = 4;
/// Maximum time to wait for EOF after the direct child exits.
///
/// A daemonized descendant can retain the slave descriptor forever. The
/// reader still drains every byte that arrives during this window, but it must
/// eventually publish an ordered terminal exit so GUI Close/Restart/Hold
/// policy cannot be held hostage by a process outside the pane's child scope.
pub const PTY_CHILD_EXIT_EOF_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
#[cfg(windows)]
const CONPTY_NONBLOCKING_WRITE_BYTES: usize = 1024;
/// Bound consecutive full-pipe waits in legacy complete-message writer APIs.
///
/// The production UI and `kettle exec` use [`PtyStdin::try_write`] and retain
/// their own pending messages. These compatibility APIs cannot return partial
/// progress without breaking their contract, so they retry for at most two
/// seconds (at one millisecond per wait) before returning/logging `WouldBlock`.
const PTY_COMPLETE_WRITE_MAX_BACKPRESSURE_RETRIES: usize = 2_000;

/// Incremental, bounded state for a fail-closed Unix canonical-EOF decision.
///
/// The stdin forwarding loop updates this state under the termios snapshot
/// used for each PTY write. Completed records release their retained memory, so
/// an arbitrarily large line-delimited stream stays bounded. Only the current
/// edited record is retained because VERASE, VWERASE, VKILL, VLNEXT, and IUTF8
/// can change whether one final VEOF or two are required. An oversized current
/// record is conservatively treated as nonempty, so it receives two VEOF
/// characters; a termios race remains ambiguous and refuses EOF injection.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct PtyInputTail {
    record: Vec<u8>,
    overflowed: bool,
    literal_next: bool,
    ambiguous_termios: bool,
    last_rules: Option<CanonicalEofRules>,
}

#[cfg(any(unix, test))]
const PTY_STDIN_EOF_TRACK_BYTES: usize = 64 * 1024;

#[cfg(any(unix, test))]
impl PtyInputTail {
    fn has_pending_state(&self) -> bool {
        self.overflowed || self.literal_next || !self.record.is_empty()
    }

    fn reset_record(&mut self) {
        self.record.clear();
        self.overflowed = false;
        self.literal_next = false;
        // A delimiter, flushing control, or transition out of canonical mode
        // proves that no earlier edited record remains. A prior termios race
        // must not poison all later, independently bounded stdin forever.
        self.ambiguous_termios = false;
    }

    fn push(&mut self, byte: u8) {
        if self.overflowed {
            return;
        }
        if self.record.len() >= PTY_STDIN_EOF_TRACK_BYTES {
            self.overflowed = true;
            self.record.clear();
            return;
        }
        self.record.push(byte);
    }

    fn erase_one(&mut self, iutf8: bool) -> Option<u8> {
        if self.overflowed {
            return None;
        }
        let mut erased = self.record.pop()?;
        if iutf8 && erased & 0xc0 == 0x80 {
            while let Some(byte) = self.record.pop() {
                erased = byte;
                if erased & 0xc0 != 0x80 {
                    break;
                }
            }
        }
        Some(erased)
    }

    fn trailing_character(&self, iutf8: bool) -> Option<u8> {
        let mut index = self.record.len().checked_sub(1)?;
        if iutf8 && self.record[index] & 0xc0 == 0x80 {
            while index > 0 && self.record[index] & 0xc0 == 0x80 {
                index -= 1;
            }
        }
        Some(self.record[index])
    }

    fn erase_word(&mut self, rules: CanonicalEofRules) {
        if self.overflowed {
            // Once the record exceeded the tracking bound, erasing any finite
            // suffix cannot prove that the kernel-side record became empty.
            return;
        }
        match rules.word_erase {
            WordEraseMode::Linux => {
                // Linux's N_TTY eraser consumes trailing punctuation/space,
                // then one ASCII-alphanumeric-or-underscore run. This differs
                // from both POSIX's whitespace word and BSD ALTWERASE.
                let mut saw_word = false;
                while let Some(byte) = self.trailing_character(rules.iutf8) {
                    let word = byte.is_ascii_alphanumeric() || byte == b'_';
                    if saw_word && !word {
                        break;
                    }
                    saw_word |= word;
                    let _ = self.erase_one(rules.iutf8);
                }
            }
            WordEraseMode::BsdSimple | WordEraseMode::BsdAlt => {
                while self
                    .record
                    .last()
                    .is_some_and(|byte| matches!(*byte, b' ' | b'\t'))
                {
                    let _ = self.erase_one(rules.iutf8);
                }
                let Some(first) = self.erase_one(rules.iutf8) else {
                    return;
                };
                let first_is_word = first.is_ascii_alphanumeric() || first == b'_' || first >= 0x80;
                while let Some(byte) = self.trailing_character(rules.iutf8) {
                    if matches!(byte, b' ' | b'\t') {
                        break;
                    }
                    if rules.word_erase == WordEraseMode::BsdAlt {
                        let is_word = byte.is_ascii_alphanumeric() || byte == b'_' || byte >= 0x80;
                        if is_word != first_is_word {
                            break;
                        }
                    }
                    let _ = self.erase_one(rules.iutf8);
                }
            }
        }
    }

    fn note_rules(&mut self, rules: CanonicalEofRules) {
        if self.last_rules.is_some_and(|previous| previous != rules) && self.has_pending_state() {
            // tcsetattr(TCSAFLUSH) and tcsetattr(TCSANOW) have the same final
            // snapshot but different effects on an existing canonical record.
            // No portable API exposes which action the child used.
            self.ambiguous_termios = true;
        }
        self.last_rules = Some(rules);
    }

    fn observe(&mut self, bytes: &[u8], rules: CanonicalEofRules) {
        self.note_rules(rules);
        if !rules.canonical {
            self.reset_record();
            return;
        }
        if rules.extproc {
            // EXTPROC bypasses the line discipline's normal canonical edit
            // and delimiter processing (and Linux publishes the bytes through
            // its noncanonical read path). Remember that forwarded data became
            // untrackable so a later EXTPROC-off transition cannot guess EOF.
            let untrackable = self.has_pending_state() || !bytes.is_empty();
            self.record.clear();
            self.overflowed = false;
            self.literal_next = false;
            self.ambiguous_termios |= untrackable;
            return;
        }

        for &raw_byte in bytes {
            let mut byte = if rules.istrip {
                raw_byte & 0x7f
            } else {
                raw_byte
            };
            if rules.iuclc && rules.iexten && byte.is_ascii_uppercase() {
                byte = byte.to_ascii_lowercase();
            }
            if byte == b'\r' {
                if rules.igncr {
                    continue;
                }
                if rules.icrnl {
                    byte = b'\n';
                }
            } else if byte == b'\n' && rules.inlcr {
                byte = b'\r';
            }

            if self.literal_next {
                self.push(byte);
                self.literal_next = false;
                continue;
            }
            if rules.iexten && rules.vlnext == Some(byte) {
                self.literal_next = true;
                continue;
            }
            if rules.ixon && (rules.vstart == Some(byte) || rules.vstop == Some(byte)) {
                continue;
            }
            if rules.isig
                && (rules.vintr == Some(byte)
                    || rules.vquit == Some(byte)
                    || rules.vsusp == Some(byte))
            {
                if !rules.noflsh {
                    self.reset_record();
                }
                continue;
            }
            if rules.verase == Some(byte) {
                let _ = self.erase_one(rules.iutf8);
                continue;
            }
            if rules.vkill == Some(byte) {
                self.reset_record();
                continue;
            }
            if rules.iexten && rules.vwerase == Some(byte) {
                self.erase_word(rules);
                continue;
            }
            if rules.iexten && (rules.vreprint == Some(byte) || rules.vdiscard == Some(byte)) {
                continue;
            }
            if byte == b'\n'
                || rules.veof == Some(byte)
                || rules.veol == Some(byte)
                || (rules.iexten && rules.veol2 == Some(byte))
            {
                self.reset_record();
                continue;
            }
            self.push(byte);
        }
    }

    fn record_unterminated(
        &mut self,
        live_rules: CanonicalEofRules,
    ) -> std::result::Result<bool, &'static str> {
        self.note_rules(live_rules);
        if self.ambiguous_termios {
            return Err("the child changed canonical termios while stdin had a pending record");
        }
        if self.overflowed {
            // The exact edited contents are no longer known, but the record
            // was definitely nonempty when it crossed the bound. Explicit
            // record delimiters, VKILL, and flushing signals reset overflow
            // while later erase controls conservatively leave it nonempty.
            return Ok(true);
        }
        if self.literal_next {
            return Err("forwarded stdin ended after the canonical VLNEXT character");
        }
        Ok(!self.record.is_empty())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct CanonicalEofRules {
    canonical: bool,
    extproc: bool,
    igncr: bool,
    icrnl: bool,
    inlcr: bool,
    istrip: bool,
    iuclc: bool,
    iexten: bool,
    iutf8: bool,
    isig: bool,
    noflsh: bool,
    ixon: bool,
    veof: Option<u8>,
    veol: Option<u8>,
    veol2: Option<u8>,
    verase: Option<u8>,
    vkill: Option<u8>,
    vlnext: Option<u8>,
    vwerase: Option<u8>,
    vreprint: Option<u8>,
    vdiscard: Option<u8>,
    vintr: Option<u8>,
    vquit: Option<u8>,
    vsusp: Option<u8>,
    vstart: Option<u8>,
    vstop: Option<u8>,
    word_erase: WordEraseMode,
}

impl CanonicalEofRules {
    #[cfg(any(unix, test))]
    fn supports_eof(self) -> bool {
        self.canonical && !self.extproc
    }
}

// Each host constructs only its own line-discipline mode; portable tests
// exercise every variant.
#[cfg_attr(not(test), allow(dead_code))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WordEraseMode {
    Linux,
    BsdSimple,
    BsdAlt,
}

#[cfg(unix)]
fn live_canonical_eof_rules(fd: RawFd) -> Result<(CanonicalEofRules, u8, u8)> {
    let mut attrs = unsafe { std::mem::zeroed::<libc::termios>() };
    if unsafe { libc::tcgetattr(fd, &mut attrs) } != 0 {
        return Err(std::io::Error::last_os_error())
            .context("cannot read live PTY termios for stdin forwarding");
    }
    let disabled = unsafe { libc::fpathconf(fd, libc::_PC_VDISABLE) };
    if !(0..=u8::MAX as libc::c_long).contains(&disabled) {
        anyhow::bail!("cannot determine the PTY's disabled control-character value");
    }
    let disabled = disabled as u8;
    let enabled_cc = |index: usize| {
        let byte = attrs.c_cc[index];
        (byte != disabled).then_some(byte)
    };
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let iuclc = attrs.c_iflag & libc::IUCLC != 0;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let iuclc = false;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let iutf8 = attrs.c_iflag & libc::IUTF8 != 0;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let iutf8 = false;
    #[cfg(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    let extproc = attrs.c_lflag & libc::EXTPROC != 0;
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )))]
    let extproc = false;
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let word_erase = WordEraseMode::Linux;
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    ))]
    let word_erase = if attrs.c_lflag & libc::ALTWERASE != 0 {
        WordEraseMode::BsdAlt
    } else {
        WordEraseMode::BsdSimple
    };
    #[cfg(not(any(
        target_os = "linux",
        target_os = "android",
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd",
        target_os = "dragonfly"
    )))]
    let word_erase = WordEraseMode::BsdSimple;
    let configured_veof = attrs.c_cc[libc::VEOF];
    Ok((
        CanonicalEofRules {
            canonical: attrs.c_lflag & libc::ICANON != 0,
            extproc,
            igncr: attrs.c_iflag & libc::IGNCR != 0,
            icrnl: attrs.c_iflag & libc::ICRNL != 0,
            inlcr: attrs.c_iflag & libc::INLCR != 0,
            istrip: attrs.c_iflag & libc::ISTRIP != 0,
            iuclc,
            iexten: attrs.c_lflag & libc::IEXTEN != 0,
            iutf8,
            isig: attrs.c_lflag & libc::ISIG != 0,
            noflsh: attrs.c_lflag & libc::NOFLSH != 0,
            ixon: attrs.c_iflag & libc::IXON != 0,
            veof: enabled_cc(libc::VEOF),
            veol: enabled_cc(libc::VEOL),
            veol2: enabled_cc(libc::VEOL2),
            verase: enabled_cc(libc::VERASE),
            vkill: enabled_cc(libc::VKILL),
            vlnext: enabled_cc(libc::VLNEXT),
            vwerase: enabled_cc(libc::VWERASE),
            vreprint: enabled_cc(libc::VREPRINT),
            vdiscard: enabled_cc(libc::VDISCARD),
            vintr: enabled_cc(libc::VINTR),
            vquit: enabled_cc(libc::VQUIT),
            vsusp: enabled_cc(libc::VSUSP),
            vstart: enabled_cc(libc::VSTART),
            vstop: enabled_cc(libc::VSTOP),
            word_erase,
        },
        configured_veof,
        disabled,
    ))
}

#[cfg(any(unix, test))]
fn pty_eof_sequence(
    canonical: bool,
    configured_veof: u8,
    disabled_value: u8,
    unterminated_record: bool,
) -> std::result::Result<Option<([u8; 2], usize)>, &'static str> {
    if !canonical {
        return Ok(None);
    }
    if configured_veof == disabled_value {
        return Err("the PTY has VEOF disabled");
    }
    let count = if unterminated_record { 2 } else { 1 };
    Ok(Some(([configured_veof; 2], count)))
}

/// The production source of this file, excluding test-only items.
#[cfg(test)]
fn production_source() -> String {
    let production = kettle_test_support::production_source(include_str!("term.rs"));
    assert!(
        !production.contains("fn production_source()"),
        "the production slice retained its own helper"
    );
    assert!(
        !production.contains("#[test]"),
        "the production slice retained a test function"
    );
    assert!(
        !production.contains("#[cfg(test)]"),
        "the production slice retained a test-only item"
    );
    production
}

#[cfg(test)]
mod pty_eof_tests {
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

    fn unterminated(
        bytes: &[u8],
        rules: CanonicalEofRules,
    ) -> std::result::Result<bool, &'static str> {
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
}

/// Delivery policy for the optional raw PTY-output side channel.
///
/// Plugins are allowed to drop chunks when their bounded queue is full. A
/// recorder or `kettle exec` requires byte-for-byte output and therefore uses
/// lossless delivery; a bounded receiver then applies backpressure before the
/// terminal lock is acquired.
pub enum PtyOutputSender {
    BestEffort(crossbeam_channel::Sender<Vec<u8>>),
    Lossless(crossbeam_channel::Sender<Vec<u8>>),
}

/// Terminal state of the blocking PTY reader.
///
/// The raw-output side channel disconnects both after an orderly EOF and after
/// an unexpected read failure. Headless callers must distinguish those cases:
/// treating every disconnect as EOF can report success with truncated output.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum PtyReadStatus {
    Reading = 0,
    Eof = 1,
    Failed = 2,
    EofTimeout = 3,
}

/// Source-side progress of the PTY read pipeline.
///
/// `generation` advances as soon as the blocking pump reads a non-empty chunk,
/// before that chunk can wait behind the bounded parser queue or a lossless
/// output subscriber. `pending_chunks` remains non-zero until the reader has
/// parsed and published each admitted chunk. Headless completion uses both so
/// ConPTY silence cannot hide an in-flight final repaint.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PtyReadProgress {
    pub status: PtyReadStatus,
    pub generation: u64,
    pub pending_chunks: usize,
}

impl PtyReadStatus {
    fn from_bits(status: u64) -> Self {
        match status {
            1 => Self::Eof,
            2 => Self::Failed,
            3 => Self::EofTimeout,
            _ => Self::Reading,
        }
    }
}

/// One atomic word makes a progress sample a real snapshot. Reading status,
/// generation, and pending work from separate atomics allowed a pump read and
/// parser completion to pass between the loads and manufacture a state that
/// never existed (old generation with zero pending work).
struct PtyReadProgressState(AtomicU64);

impl PtyReadProgressState {
    const STATUS_MASK: u64 = 0b11;
    const PENDING_SHIFT: u32 = 2;
    const PENDING_BITS: u32 = 14;
    const PENDING_ONE: u64 = 1 << Self::PENDING_SHIFT;
    const PENDING_MASK: u64 = ((1 << Self::PENDING_BITS) - 1) << Self::PENDING_SHIFT;
    const GENERATION_SHIFT: u32 = Self::PENDING_SHIFT + Self::PENDING_BITS;
    const GENERATION_ONE: u64 = 1 << Self::GENERATION_SHIFT;

    fn new() -> Self {
        Self(AtomicU64::new(0))
    }

    fn load(&self) -> PtyReadProgress {
        let state = self.0.load(Ordering::Acquire);
        PtyReadProgress {
            status: PtyReadStatus::from_bits(state & Self::STATUS_MASK),
            generation: state >> Self::GENERATION_SHIFT,
            pending_chunks: ((state & Self::PENDING_MASK) >> Self::PENDING_SHIFT) as usize,
        }
    }

    fn set_status(&self, status: PtyReadStatus) {
        let _ = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                Some((state & !Self::STATUS_MASK) | status as u64)
            });
    }

    fn mark_chunk_read(&self) {
        let _ = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                let pending = (state & Self::PENDING_MASK) >> Self::PENDING_SHIFT;
                assert!(
                    pending < (1 << Self::PENDING_BITS) - 1,
                    "PTY pending-chunk counter overflowed"
                );
                Some(state.wrapping_add(Self::GENERATION_ONE + Self::PENDING_ONE))
            });
    }

    fn mark_chunk_handled(&self) {
        let _ = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                let pending = (state & Self::PENDING_MASK) >> Self::PENDING_SHIFT;
                debug_assert!(
                    pending != 0,
                    "a PTY parser chunk was not tracked by the pump"
                );
                (pending != 0).then_some(state - Self::PENDING_ONE)
            });
    }
}

fn pty_read_error_status(error: &io::Error) -> PtyReadStatus {
    #[cfg(unix)]
    if error.raw_os_error() == Some(libc::EIO) {
        // Unix PTY masters commonly report the final slave close as EIO
        // instead of an ordinary zero-byte read.
        return PtyReadStatus::Eof;
    }
    #[cfg(windows)]
    if error.kind() == io::ErrorKind::BrokenPipe
        || error.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_BROKEN_PIPE as i32)
        || error.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_NO_DATA as i32)
    {
        return PtyReadStatus::Eof;
    }
    PtyReadStatus::Failed
}

/// Observe a direct Unix child without consuming its wait status.
///
/// The PTY startup guard uses this while it temporarily retains Kettle's
/// slave descriptor: a silent child must be allowed to close the master, but a
/// child whose output has not reached the reader yet must keep that output
/// anchored. `WNOWAIT` leaves the actual reap to the existing child owner.
#[cfg(unix)]
fn unix_child_exit_code_unreaped(pid: u32) -> io::Result<Option<u32>> {
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| io::Error::other("PTY child process id is out of range"))?;

    // SAFETY: `info` is initialized for the kernel, `pid` identifies this
    // process's direct child, and WNOWAIT explicitly leaves its wait status
    // available to portable-pty's later `try_wait`.
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    loop {
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as libc::id_t,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            // POSIX reports a successful WNOHANG probe with si_pid == 0 while
            // the selected child is still running.
            if unsafe { info.si_pid() } == 0 {
                return Ok(None);
            }
            let status = unsafe { info.si_status() };
            let code = match info.si_code {
                libc::CLD_EXITED => status as u32,
                libc::CLD_KILLED | libc::CLD_DUMPED => 128u32.saturating_add(status as u32),
                // WEXITED excludes stop/continue notifications. Treat an
                // unexpected code as an observation failure, not success.
                other => {
                    return Err(io::Error::other(format!(
                        "waitid returned unexpected child status code {other}"
                    )));
                }
            };
            return Ok(Some(code));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(unix)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum UnixStartupWake {
    Output,
    ChildExit,
    OutputAndChildExit,
}

/// Event-driven PTY readability and direct-child exit observation.
///
/// Linux exposes child exit as a pollable pidfd; macOS exposes it through
/// EVFILT_PROC. Both can wait alongside master readability without a timer.
/// The same watcher first protects Kettle's retained startup slave, then stays
/// with the pump so a descendant cannot retain its slave forever after the
/// direct child exits. Other Unix targets retain a bounded, exponentially
/// backed-off fallback so portability does not turn a quiet pane into a 20 Hz
/// wakeup loop.
#[cfg(unix)]
struct UnixPtyWatcher {
    pid: u32,
    fallback_timeout_ms: i32,
    #[cfg(target_os = "linux")]
    pidfd: Option<OwnedFd>,
    #[cfg(target_os = "macos")]
    kqueue: Option<OwnedFd>,
}

#[cfg(unix)]
impl UnixPtyWatcher {
    fn new(pid: u32, _master_fd: RawFd) -> Self {
        #[cfg(target_os = "linux")]
        let pidfd = {
            let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
            if raw < 0 {
                log::debug!(
                    "pidfd_open unavailable for PTY startup guard; using bounded fallback: {}",
                    io::Error::last_os_error()
                );
                None
            } else {
                // SAFETY: a successful pidfd_open returns a new descriptor
                // owned by this watcher.
                Some(unsafe { OwnedFd::from_raw_fd(raw as RawFd) })
            }
        };

        #[cfg(target_os = "macos")]
        let kqueue = Self::macos_kqueue(pid, _master_fd).map_or_else(
            |error| {
                log::debug!(
                    "kqueue unavailable for PTY startup guard; using bounded fallback: {error}"
                );
                None
            },
            Some,
        );

        Self {
            pid,
            fallback_timeout_ms: 1,
            #[cfg(target_os = "linux")]
            pidfd,
            #[cfg(target_os = "macos")]
            kqueue,
        }
    }

    #[cfg(target_os = "macos")]
    fn macos_kqueue(pid: u32, master_fd: RawFd) -> io::Result<OwnedFd> {
        let raw = unsafe { libc::kqueue() };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: kqueue returned a new descriptor owned by this function.
        let queue = unsafe { OwnedFd::from_raw_fd(raw) };
        let changes = [
            libc::kevent {
                ident: master_fd as libc::uintptr_t,
                filter: libc::EVFILT_READ,
                flags: libc::EV_ADD | libc::EV_ENABLE,
                fflags: 0,
                data: 0,
                udata: std::ptr::null_mut(),
            },
            libc::kevent {
                ident: pid as libc::uintptr_t,
                filter: libc::EVFILT_PROC,
                flags: libc::EV_ADD | libc::EV_ENABLE,
                fflags: libc::NOTE_EXIT,
                data: 0,
                udata: std::ptr::null_mut(),
            },
        ];
        let registered = unsafe {
            libc::kevent(
                queue.as_raw_fd(),
                changes.as_ptr(),
                changes.len() as i32,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if registered < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(queue)
    }

    fn wait(
        &mut self,
        master_fd: RawFd,
        deadline: Option<std::time::Instant>,
    ) -> io::Result<Option<UnixStartupWake>> {
        #[cfg(target_os = "linux")]
        if let Some(pidfd) = &self.pidfd {
            let mut fds = [
                libc::pollfd {
                    fd: master_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: pidfd.as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            loop {
                let timeout = deadline.map(poll_timeout_ms_until).unwrap_or(-1);
                let result = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as _, timeout) };
                if result == 0 {
                    return Ok(None);
                }
                if result >= 0 {
                    let output =
                        fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0;
                    let child_exit = fds[1].revents != 0;
                    if output && child_exit {
                        return Ok(Some(UnixStartupWake::OutputAndChildExit));
                    }
                    if output {
                        return Ok(Some(UnixStartupWake::Output));
                    }
                    if child_exit {
                        return Ok(Some(UnixStartupWake::ChildExit));
                    }
                    return Err(io::Error::other(
                        "PTY startup poll woke without output or child exit",
                    ));
                }
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }

        #[cfg(target_os = "macos")]
        if let Some(queue) = &self.kqueue {
            let mut events: [libc::kevent; 2] = unsafe { std::mem::zeroed() };
            loop {
                let timeout = deadline.map(kqueue_timeout_until);
                let count = unsafe {
                    libc::kevent(
                        queue.as_raw_fd(),
                        std::ptr::null(),
                        0,
                        events.as_mut_ptr(),
                        events.len() as i32,
                        timeout
                            .as_ref()
                            .map_or(std::ptr::null(), |value| value as *const libc::timespec),
                    )
                };
                if count == 0 {
                    return Ok(None);
                }
                if count >= 0 {
                    let mut output = false;
                    let mut child_exit = false;
                    for event in events.iter().take(count as usize) {
                        let flags = event.flags;
                        let data = event.data;
                        if flags & libc::EV_ERROR != 0 && data != 0 {
                            return Err(io::Error::from_raw_os_error(data as i32));
                        }
                        let filter = event.filter;
                        if filter == libc::EVFILT_READ {
                            output = true;
                        }
                        if filter == libc::EVFILT_PROC {
                            let fflags = event.fflags;
                            child_exit |= fflags & libc::NOTE_EXIT != 0;
                        }
                    }
                    if output && child_exit {
                        return Ok(Some(UnixStartupWake::OutputAndChildExit));
                    }
                    if output {
                        return Ok(Some(UnixStartupWake::Output));
                    }
                    if child_exit {
                        return Ok(Some(UnixStartupWake::ChildExit));
                    }
                    return Err(io::Error::other(
                        "PTY startup kqueue woke without output or child exit",
                    ));
                }
                let error = io::Error::last_os_error();
                if error.kind() != io::ErrorKind::Interrupted {
                    return Err(error);
                }
            }
        }

        self.wait_fallback(master_fd, deadline)
    }

    fn wait_fallback(
        &mut self,
        master_fd: RawFd,
        deadline: Option<std::time::Instant>,
    ) -> io::Result<Option<UnixStartupWake>> {
        loop {
            let mut poll_fd = libc::pollfd {
                fd: master_fd,
                events: libc::POLLIN,
                revents: 0,
            };
            let timeout = deadline
                .map(poll_timeout_ms_until)
                .map_or(self.fallback_timeout_ms, |remaining| {
                    remaining.min(self.fallback_timeout_ms)
                });
            let result = unsafe { libc::poll(&mut poll_fd, 1, timeout) };
            if result > 0 && poll_fd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                return Ok(Some(UnixStartupWake::Output));
            }
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                return Ok(None);
            }
            if unix_child_exit_code_unreaped(self.pid)?.is_some() {
                return Ok(Some(UnixStartupWake::ChildExit));
            }
            self.fallback_timeout_ms = (self.fallback_timeout_ms * 2).min(1_000);
        }
    }
}

#[cfg(unix)]
fn poll_timeout_ms_until(deadline: std::time::Instant) -> i32 {
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    if remaining.is_zero() {
        return 0;
    }
    remaining
        .as_millis()
        .saturating_add(1)
        .min(i32::MAX as u128) as i32
}

#[cfg(unix)]
fn wait_for_master_until(master_fd: RawFd, deadline: std::time::Instant) -> io::Result<bool> {
    loop {
        let mut fd = libc::pollfd {
            fd: master_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut fd, 1, poll_timeout_ms_until(deadline)) };
        if result == 0 {
            return Ok(false);
        }
        if result > 0 {
            if fd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                return Ok(true);
            }
            return Err(io::Error::other(
                "PTY master poll woke without readability, hangup, or error",
            ));
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

#[cfg(target_os = "macos")]
fn kqueue_timeout_until(deadline: std::time::Instant) -> libc::timespec {
    let remaining = deadline.saturating_duration_since(std::time::Instant::now());
    libc::timespec {
        tv_sec: remaining.as_secs().min(libc::time_t::MAX as u64) as libc::time_t,
        tv_nsec: remaining.subsec_nanos() as libc::c_long,
    }
}

/// Wait for the direct ConPTY child without consuming its status or holding
/// the `Child` mutex. The duplicate process handle remains signalled after
/// exit, while the original handle stays with `Terminal` for ordinary status
/// collection and teardown.
#[cfg(windows)]
fn spawn_windows_child_exit_observer(
    child: &(dyn portable_pty::Child + Send + Sync),
    observed_at: Arc<Mutex<Option<std::time::Instant>>>,
    lifecycle_pending: Arc<AtomicBool>,
    waker: Waker,
) -> io::Result<()> {
    use std::os::windows::io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle};
    use windows_sys::Win32::Foundation::{
        DUPLICATE_SAME_ACCESS, DuplicateHandle, HANDLE, WAIT_FAILED, WAIT_OBJECT_0,
    };
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, INFINITE, WaitForSingleObject};

    let source = child
        .as_raw_handle()
        .ok_or_else(|| io::Error::other("ConPTY child has no process handle"))?;
    let process = unsafe { GetCurrentProcess() };
    let mut duplicate: HANDLE = std::ptr::null_mut();
    let duplicated = unsafe {
        DuplicateHandle(
            process,
            source as HANDLE,
            process,
            &mut duplicate,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if duplicated == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: DuplicateHandle returned a new owned process handle.
    let handle = unsafe { OwnedHandle::from_raw_handle(duplicate) };
    std::thread::Builder::new()
        .name("kettle-child-observer".into())
        .spawn(move || {
            let result = unsafe { WaitForSingleObject(handle.as_raw_handle() as HANDLE, INFINITE) };
            match result {
                WAIT_OBJECT_0 => {
                    let now = std::time::Instant::now();
                    if let Ok(mut slot) = observed_at.lock() {
                        *slot = Some(now);
                    }
                    lifecycle_pending.store(true, Ordering::Release);
                    waker();
                }
                WAIT_FAILED => {
                    log::error!(
                        "ConPTY child observer wait failed: {}",
                        io::Error::last_os_error()
                    );
                }
                other => log::error!("ConPTY child observer returned unexpected status {other}"),
            }
        })
        .map(|_| ())
}

impl PtyOutputSender {
    pub fn best_effort(sender: crossbeam_channel::Sender<Vec<u8>>) -> Self {
        Self::BestEffort(sender)
    }

    pub fn lossless(sender: crossbeam_channel::Sender<Vec<u8>>) -> Self {
        Self::Lossless(sender)
    }

    fn send(&self, chunk: Vec<u8>) {
        match self {
            Self::BestEffort(sender) => {
                let _ = sender.try_send(chunk);
            }
            Self::Lossless(sender) => {
                let _ = sender.send(chunk);
            }
        }
    }
}

/// Exact PTY grid and text-area pixel geometry.
///
/// Keeping total pixels instead of a rounded integer cell size matters at
/// fractional display scales: 100 columns at 9.6 px are 960 px, not 1000 px.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PtyGeometry {
    pub columns: usize,
    pub rows: usize,
    pub pixel_width: u16,
    pub pixel_height: u16,
}

/// Runtime capabilities that influence terminal protocol replies.
///
/// These are separate from the parser's compiled feature set: a capability
/// such as OSC 52 can be present in the binary but unavailable under the
/// active security policy or on a headless platform.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminalCapabilities {
    pub osc52_copy: bool,
    pub unnegotiated_modified_enter: bool,
    /// Spawn the command inside an OS-owned descendant containment boundary.
    /// Currently meaningful for headless automation on Windows.
    pub contain_process_tree: bool,
    /// Observe direct-child exit independently of PTY EOF for interactive UI
    /// drain policy. Headless exec already owns process-status polling and
    /// must not allocate a redundant observer.
    pub observe_child_exit: bool,
}

impl Default for TerminalCapabilities {
    fn default() -> Self {
        Self {
            osc52_copy: true,
            unnegotiated_modified_enter: true,
            contain_process_tree: false,
            observe_child_exit: false,
        }
    }
}

/// How an explicitly supplied child working directory is handled.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum WorkingDirectoryPolicy {
    /// Interactive panes recover a stale saved directory by starting in HOME.
    #[default]
    FallbackToHome,
    /// Automation must pass the explicit path to the OS unchanged so a stale
    /// or invalid directory fails the spawn instead of relocating the command.
    RejectInvalidExplicit,
}

impl PtyGeometry {
    pub fn new(columns: usize, rows: usize, pixel_width: u16, pixel_height: u16) -> Self {
        Self {
            columns: columns.max(1),
            rows: rows.max(1),
            pixel_width: pixel_width.max(1),
            pixel_height: pixel_height.max(1),
        }
    }

    pub fn from_cell_size(columns: usize, rows: usize, cell_width: u16, cell_height: u16) -> Self {
        Self::new(
            columns,
            rows,
            clamp_pty_dim(cell_width.max(1), columns),
            clamp_pty_dim(cell_height.max(1), rows),
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct VersionedPtyGeometry {
    geometry: PtyGeometry,
    generation: u64,
}

/// Convert a pixel extent to the number of cells it occupies using the exact
/// total PTY geometry. Multiplying before dividing preserves fractional cell
/// widths (for example, 960 px / 100 columns = 9.6 px per cell) without using
/// floating-point arithmetic on the PTY reader thread.
fn image_cells_for_pixels(pixels: u32, cells: usize, total_pixels: u16) -> usize {
    if pixels == 0 || cells == 0 {
        return 0;
    }
    let numerator = u64::from(pixels).saturating_mul(cells.min(u64::MAX as usize) as u64);
    let occupied = numerator.div_ceil(u64::from(total_pixels.max(1)));
    usize::try_from(occupied).unwrap_or(usize::MAX)
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ResolvedKittyPlacement {
    source_rect: Option<ImageSourceRect>,
    cell_cols: usize,
    cell_rows: usize,
    x_offset_cells: f32,
    y_offset_cells: f32,
    display_cols: f32,
    display_rows: f32,
}

/// Resolve Kitty's raw placement rectangle against the current cell geometry.
///
/// `effective_num_{cols,rows}` (used for cursor movement and deletion bounds)
/// intentionally differs from the exact rendered rectangle when `X`/`Y` is
/// non-zero. This mirrors Kitty's `update_dest_rect` and layer construction:
/// offsets count toward auto-sized occupied cells, while an explicitly sized
/// destination ends at the requested cell boundary.
fn resolve_kitty_placement(
    image: &crate::ImageData,
    params: PlacementParams,
    geometry: PtyGeometry,
) -> Option<ResolvedKittyPlacement> {
    let source_x = params.source_x.min(image.width);
    let source_y = params.source_y.min(image.height);
    let available_width = image.width.saturating_sub(source_x);
    let available_height = image.height.saturating_sub(source_y);
    let source_width = if params.source_width == 0 {
        available_width
    } else {
        params.source_width.min(available_width)
    };
    let source_height = if params.source_height == 0 {
        available_height
    } else {
        params.source_height.min(available_height)
    };
    if source_width == 0 || source_height == 0 {
        return None;
    }

    let cell_width = f64::from(geometry.pixel_width) / geometry.columns.max(1) as f64;
    let cell_height = f64::from(geometry.pixel_height) / geometry.rows.max(1) as f64;
    if !cell_width.is_finite()
        || !cell_height.is_finite()
        || cell_width <= 0.0
        || cell_height <= 0.0
    {
        return None;
    }

    // Protocol offsets are integral pixels and must be smaller than a cell.
    // `ceil(cell)-1` is the largest integral offset below a fractional cell
    // extent (for example, 9 for a 9.6 px cell).
    let max_x_offset = (cell_width.ceil() - 1.0).max(0.0);
    let max_y_offset = (cell_height.ceil() - 1.0).max(0.0);
    let x_offset = f64::from(params.cell_x_offset).min(max_x_offset);
    let y_offset = f64::from(params.cell_y_offset).min(max_y_offset);
    let x_offset_cells = x_offset / cell_width;
    let y_offset_cells = y_offset / cell_height;
    let source_width_px = f64::from(source_width);
    let source_height_px = f64::from(source_height);

    let auto_cols = params.columns == 0;
    let auto_rows = params.rows == 0;
    let cell_cols = if auto_cols {
        let width_px = if auto_rows {
            source_width_px + x_offset
        } else {
            let height_px = cell_height * f64::from(params.rows) + y_offset;
            height_px * source_width_px / source_height_px
        };
        ceil_f64_to_usize(width_px / cell_width)
    } else {
        usize::try_from(params.columns).unwrap_or(usize::MAX)
    };
    let cell_rows = if auto_rows {
        let height_px = if auto_cols {
            source_height_px + y_offset
        } else {
            let width_px = cell_width * f64::from(params.columns) + x_offset;
            width_px * source_height_px / source_width_px
        };
        ceil_f64_to_usize(height_px / cell_height)
    } else {
        usize::try_from(params.rows).unwrap_or(usize::MAX)
    };
    if cell_cols == 0 || cell_rows == 0 {
        return None;
    }

    let display_cols = if auto_cols {
        if auto_rows {
            source_width_px / cell_width
        } else {
            let display_height = f64::from(params.rows) - y_offset_cells;
            display_height * cell_height * source_width_px / source_height_px / cell_width
        }
    } else {
        f64::from(params.columns) - x_offset_cells
    };
    let display_rows = if auto_rows {
        let display_width = if auto_cols {
            source_width_px / cell_width
        } else {
            f64::from(params.columns) - x_offset_cells
        };
        display_width * cell_width * source_height_px / source_width_px / cell_height
    } else {
        f64::from(params.rows) - y_offset_cells
    };
    if !display_cols.is_finite()
        || !display_rows.is_finite()
        || display_cols <= 0.0
        || display_rows <= 0.0
    {
        return None;
    }

    let source_rect = (source_x != 0
        || source_y != 0
        || source_width != image.width
        || source_height != image.height)
        .then_some(ImageSourceRect {
            x: source_x,
            y: source_y,
            width: source_width,
            height: source_height,
        });
    Some(ResolvedKittyPlacement {
        source_rect,
        cell_cols,
        cell_rows,
        x_offset_cells: x_offset_cells as f32,
        y_offset_cells: y_offset_cells as f32,
        display_cols: display_cols as f32,
        display_rows: display_rows as f32,
    })
}

fn ceil_f64_to_usize(value: f64) -> usize {
    if !value.is_finite() || value <= 0.0 {
        0
    } else if value >= usize::MAX as f64 {
        usize::MAX
    } else {
        value.ceil() as usize
    }
}

fn kitty_cursor_movement(
    params: PlacementParams,
    cell_cols: usize,
    cell_rows: usize,
) -> Option<String> {
    if params.suppress_cursor_movement {
        return None;
    }
    let down = cell_rows.saturating_sub(1);
    if down == 0 {
        Some(format!("\x1b[{cell_cols}C"))
    } else {
        Some(format!("\x1b[{cell_cols}C\x1b[{down}B"))
    }
}

fn recompute_kitty_placements(placements: &mut Vec<Placement>, geometry: PtyGeometry) {
    placements.retain_mut(|placement| {
        let Some(params) = placement.kitty_params else {
            return true;
        };
        let Some(resolved) = resolve_kitty_placement(&placement.img, params, geometry) else {
            return false;
        };
        placement.cell_cols = resolved.cell_cols;
        placement.x_offset_cells = resolved.x_offset_cells;
        placement.display_cols = resolved.display_cols;
        placement.source_rect = resolved.source_rect;

        if let Some(crop) = placement.source_crop {
            if !crop.top.is_finite()
                || !crop.bottom.is_finite()
                || crop.top < 0.0
                || crop.top >= crop.bottom
                || crop.bottom > 1.0
            {
                return false;
            }

            // `source_crop` is already composed in the original source
            // rectangle's normalized coordinates. Re-resolve the raw Kitty
            // request for the new cell geometry, then retain only that
            // permanent fragment. Its post-scroll document anchor and
            // fractional y offset are intentionally preserved; resetting the
            // offset from `params.cell_y_offset` would visibly jump it.
            let retained_fraction = crop.bottom - crop.top;
            placement.display_rows = resolved.display_rows * retained_fraction;
            refresh_placement_cell_rows(placement)
        } else {
            placement.cell_rows = resolved.cell_rows;
            placement.y_offset_cells = resolved.y_offset_cells;
            placement.display_rows = resolved.display_rows;
            true
        }
    });
}

/// A `Write` sink that discards everything. On `Terminal`
/// teardown the PTY writer (the child's stdin / conin) is swapped for this
/// so dropping the real writer closes the input handle immediately — an EOF
/// nudge for shells that exit on stdin close — without leaving the field
/// holding a dangling handle. Zero-sized; never errors.
struct NullWrite;

impl std::io::Write for NullWrite {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Avoid rescanning the bounded placement registry while the retained-history
/// origin is unchanged.
#[derive(Default)]
struct ImageHistoryPruner {
    /// Primary and alternate grids advance independent origins which can have
    /// the same numeric value. Include the active-buffer identity so restoring
    /// a resized primary registry can never inherit the alternate cache hit.
    last_key: Option<(bool, u64)>,
}

impl ImageHistoryPruner {
    fn prune_if_changed(&mut self, term: &SharedTerm, images: &Images) {
        let key = {
            let term = term.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            (
                term.mode()
                    .contains(alacritty_terminal::term::TermMode::ALT_SCREEN),
                term.grid().history_origin(),
            )
        };
        if self.last_key == Some(key) {
            return;
        }
        prune(images, key.1);
        self.last_key = Some(key);
    }
}

#[derive(Default)]
struct BufferGraphicsState {
    placements: Vec<Placement>,
    virtuals: std::collections::HashMap<(u32, u32), VirtualEntry>,
    animations: std::collections::HashMap<u32, AnimEntry>,
    relatives: std::collections::HashMap<(u32, u32), RelEntry>,
}

type InactiveGraphics = Arc<Mutex<BufferGraphicsState>>;

impl BufferGraphicsState {
    fn take_from(
        images: &Images,
        virtuals: &Virtuals,
        anims: &Animations,
        relatives: &Relatives,
    ) -> Self {
        Self {
            placements: std::mem::take(
                &mut *images
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            ),
            virtuals: std::mem::take(
                &mut *virtuals
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            ),
            animations: std::mem::take(
                &mut *anims
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            ),
            relatives: std::mem::take(
                &mut *relatives
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()),
            ),
        }
    }

    fn install_into(
        self,
        images: &Images,
        virtuals: &Virtuals,
        anims: &Animations,
        relatives: &Relatives,
    ) {
        *images
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = self.placements;
        *virtuals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = self.virtuals;
        *anims
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = self.animations;
        *relatives
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = self.relatives;
    }

    fn clear_regular_placements(&mut self) {
        self.placements.clear();
        self.relatives.clear();
    }
}

fn clear_active_graphics(
    images: &Images,
    virtuals: &Virtuals,
    anims: &Animations,
    relatives: &Relatives,
) {
    BufferGraphicsState::default().install_into(images, virtuals, anims, relatives);
}

fn clear_reflowed_regular_placements(
    images: &Images,
    relatives: &Relatives,
    inactive: &InactiveGraphics,
) {
    images
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    relatives
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear();
    inactive
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clear_regular_placements();
}

#[derive(Clone, Copy)]
struct GraphicsRegistries<'a> {
    inactive: &'a InactiveGraphics,
    images: &'a Images,
    virtuals: &'a Virtuals,
    anims: &'a Animations,
    relatives: &'a Relatives,
}

fn placement_viewport_start(placement: &Placement, screen_top: u64) -> f64 {
    let line = if placement.abs_line >= screen_top {
        placement.abs_line.saturating_sub(screen_top) as f64
    } else {
        -(screen_top.saturating_sub(placement.abs_line) as f64)
    };
    line + f64::from(placement.y_offset_cells)
}

fn set_placement_viewport_start(
    placement: &mut Placement,
    screen_top: u64,
    viewport_start: f64,
) -> bool {
    if !viewport_start.is_finite()
        || viewport_start < i64::MIN as f64
        || viewport_start > i64::MAX as f64
    {
        return false;
    }

    let row = viewport_start.floor() as i128;
    let fraction = viewport_start - row as f64;
    let absolute = i128::from(screen_top).saturating_add(row);
    if absolute < 0 {
        placement.abs_line = 0;
        placement.y_offset_cells = (absolute as f64 + fraction) as f32;
    } else {
        placement.abs_line = u64::try_from(absolute).unwrap_or(u64::MAX);
        placement.y_offset_cells = fraction as f32;
    }

    refresh_placement_cell_rows(placement)
}

fn refresh_placement_cell_rows(placement: &mut Placement) -> bool {
    let occupied_rows =
        (f64::from(placement.y_offset_cells) + f64::from(placement.display_rows)).ceil();
    if !occupied_rows.is_finite() || occupied_rows <= 0.0 {
        return false;
    }
    placement.cell_rows = if occupied_rows >= usize::MAX as f64 {
        usize::MAX
    } else {
        occupied_rows as usize
    };
    true
}

fn retain_source_vertical_range(
    placement: &mut Placement,
    retained_top: f64,
    retained_bottom: f64,
) -> bool {
    if !retained_top.is_finite()
        || !retained_bottom.is_finite()
        || retained_top < 0.0
        || retained_top >= retained_bottom
        || retained_bottom > 1.0
    {
        return false;
    }
    if retained_top == 0.0 && retained_bottom == 1.0 {
        return true;
    }

    let existing = placement.source_crop.unwrap_or(ImageSourceCrop {
        top: 0.0,
        bottom: 1.0,
    });
    if !existing.top.is_finite()
        || !existing.bottom.is_finite()
        || existing.top < 0.0
        || existing.top >= existing.bottom
        || existing.bottom > 1.0
    {
        return false;
    }
    let span = f64::from(existing.bottom - existing.top);
    let top = f64::from(existing.top) + span * retained_top;
    let bottom = f64::from(existing.top) + span * retained_bottom;
    if top >= bottom {
        return false;
    }
    placement.source_crop = Some(ImageSourceCrop {
        top: top as f32,
        bottom: bottom as f32,
    });
    true
}

#[derive(Clone, Copy)]
struct GraphicsScroll {
    direction: GraphicsScrollDirection,
    top: usize,
    bottom: usize,
    lines: usize,
    old_screen_top: u64,
    new_screen_top: u64,
    screen_lines: usize,
}

fn scroll_regular_placements(images: &Images, scroll: GraphicsScroll) -> bool {
    let GraphicsScroll {
        direction,
        top,
        bottom,
        lines,
        old_screen_top,
        new_screen_top,
        screen_lines,
    } = scroll;
    if top >= bottom || bottom > screen_lines || lines == 0 || lines > bottom - top {
        return false;
    }

    let region_top = top as f64;
    let region_bottom = bottom as f64;
    let viewport_bottom = screen_lines as f64;
    let scrolls_into_history =
        direction == GraphicsScrollDirection::Up && top == 0 && new_screen_top > old_screen_top;
    let displacement = match direction {
        GraphicsScrollDirection::Up if scrolls_into_history => {
            -(new_screen_top.saturating_sub(old_screen_top) as f64)
        }
        GraphicsScrollDirection::Up => -(lines as f64),
        GraphicsScrollDirection::Down => lines as f64,
    };
    let mut placements = images
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    placements.retain_mut(|placement| {
        let start = placement_viewport_start(placement, old_screen_top);
        let height = f64::from(placement.display_rows);
        let end = start + height;
        if !start.is_finite() || !height.is_finite() || height <= 0.0 || !end.is_finite() {
            return false;
        }

        // Placements wholly outside the active viewport remain history-owned.
        if end <= 0.0 || start >= viewport_bottom {
            return true;
        }

        // Kitty scrolls only images entirely contained by the page margins.
        // Everything else remains at the same visual row; re-anchoring is
        // still required when a top-anchored region advances stable row ids.
        if start < region_top || end > region_bottom {
            return set_placement_viewport_start(placement, new_screen_top, start);
        }

        let moved_start = start + displacement;
        let moved_end = end + displacement;
        let retained_start = if scrolls_into_history {
            moved_start
        } else {
            moved_start.max(region_top)
        };
        let retained_end = moved_end.min(region_bottom);
        if retained_start >= retained_end {
            return false;
        }

        let retained_top = (retained_start - moved_start) / height;
        let retained_bottom = (retained_end - moved_start) / height;
        if !retain_source_vertical_range(placement, retained_top, retained_bottom) {
            return false;
        }
        placement.display_rows = (retained_end - retained_start) as f32;
        set_placement_viewport_start(placement, new_screen_top, retained_start)
    });
    true
}

fn reset_all_graphics_to_screen(
    active_alternate: &mut bool,
    registries: GraphicsRegistries<'_>,
    extractor: &mut Extractor,
) {
    let GraphicsRegistries {
        inactive,
        images,
        virtuals,
        anims,
        relatives,
    } = registries;
    clear_active_graphics(images, virtuals, anims, relatives);
    *inactive
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = BufferGraphicsState::default();
    extractor.reset_all_graphics();
    if *active_alternate {
        extractor.enter_alternate_screen(false);
    }
}

fn apply_graphics_event(
    event: GraphicsEvent,
    active_alternate: &mut bool,
    registries: GraphicsRegistries<'_>,
    extractor: &mut Extractor,
) -> bool {
    let GraphicsRegistries {
        inactive,
        images,
        virtuals,
        anims,
        relatives,
    } = registries;
    match event {
        GraphicsEvent::EraseDisplay => {
            clear_active_graphics(images, virtuals, anims, relatives);
            extractor.clear_active_graphics();
            true
        }
        GraphicsEvent::EnterAlternate { clear, .. } if !*active_alternate => {
            let active = BufferGraphicsState::take_from(images, virtuals, anims, relatives);
            let alternate = {
                let mut inactive = inactive
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if clear {
                    *inactive = BufferGraphicsState::default();
                }
                std::mem::replace(&mut *inactive, active)
            };
            alternate.install_into(images, virtuals, anims, relatives);
            extractor.enter_alternate_screen(clear);
            *active_alternate = true;
            true
        }
        GraphicsEvent::LeaveAlternate { clear, .. } if *active_alternate => {
            if clear {
                clear_active_graphics(images, virtuals, anims, relatives);
            }
            let active = BufferGraphicsState::take_from(images, virtuals, anims, relatives);
            let primary = {
                let mut inactive = inactive
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                std::mem::replace(&mut *inactive, active)
            };
            primary.install_into(images, virtuals, anims, relatives);
            extractor.leave_alternate_screen(clear);
            *active_alternate = false;
            true
        }
        GraphicsEvent::Reset => {
            clear_active_graphics(images, virtuals, anims, relatives);
            *inactive
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner()) = BufferGraphicsState::default();
            extractor.reset_all_graphics();
            *active_alternate = false;
            true
        }
        GraphicsEvent::Scroll {
            direction,
            top,
            bottom,
            lines,
            old_screen_top,
            new_screen_top,
            screen_lines,
        } => scroll_regular_placements(
            images,
            GraphicsScroll {
                direction,
                top,
                bottom,
                lines,
                old_screen_top,
                new_screen_top,
                screen_lines,
            },
        ),
        GraphicsEvent::SyncMarker { .. }
        | GraphicsEvent::EnterAlternate { .. }
        | GraphicsEvent::LeaveAlternate { .. } => false,
    }
}

fn apply_graphics_batch(
    batch: GraphicsEventBatch,
    active_alternate: &mut bool,
    registries: GraphicsRegistries<'_>,
    extractor: &mut Extractor,
) -> bool {
    if batch.overflowed {
        *active_alternate = batch.alternate_screen;
        reset_all_graphics_to_screen(active_alternate, registries, extractor);
        return false;
    }

    let mut consistent = true;
    for event in batch.events {
        consistent &= apply_graphics_event(event, active_alternate, registries, extractor);
        if !consistent {
            break;
        }
    }
    if !consistent || *active_alternate != batch.alternate_screen {
        *active_alternate = batch.alternate_screen;
        reset_all_graphics_to_screen(active_alternate, registries, extractor);
        false
    } else {
        true
    }
}

struct DeferredGraphicsJournal {
    entries: VecDeque<(u64, DeferredGraphics)>,
    next_id: u64,
    overflowed: bool,
}

impl DeferredGraphicsJournal {
    fn new() -> Self {
        Self {
            entries: VecDeque::with_capacity(SYNC_MARKER_CAPACITY),
            next_id: 0,
            overflowed: false,
        }
    }

    fn defer(&mut self, processor: &mut Processor, graphics: DeferredGraphics) {
        if self.overflowed {
            return;
        }
        let Some(next_id) = self.next_id.checked_add(1) else {
            self.fail_closed();
            return;
        };
        if self.entries.len() == SYNC_MARKER_CAPACITY || !processor.push_sync_marker(self.next_id) {
            self.fail_closed();
            return;
        }
        self.entries.push_back((self.next_id, graphics));
        self.next_id = next_id;
    }

    fn take(&mut self, id: u64) -> Option<DeferredGraphics> {
        if self.overflowed {
            return None;
        }
        match self.entries.pop_front() {
            Some((queued_id, graphics)) if queued_id == id => Some(graphics),
            _ => {
                self.fail_closed();
                None
            }
        }
    }

    fn fail_closed(&mut self) {
        self.entries.clear();
        self.overflowed = true;
    }

    fn finish_sync(&mut self) -> bool {
        let consistent = !self.overflowed && self.entries.is_empty();
        self.entries.clear();
        self.overflowed = false;
        consistent
    }
}

fn chunk_needs_graphics_gate(chunk: &Chunk) -> bool {
    // Pass bytes are not graphics-free: LF, CSI scrolling/erase, alternate
    // screen switches, and RIS can all commit GraphicsEvents. Hold the gate
    // across both the terminal mutation and journal application so a resize
    // cannot observe and reorder the boundary between them.
    matches!(
        chunk,
        Chunk::Pass(_)
            | Chunk::Terminal(_)
            | Chunk::Image(_)
            | Chunk::DeleteImages(_)
            | Chunk::VirtualImage { .. }
            | Chunk::RelativePlacement { .. }
            | Chunk::Animation { .. }
            | Chunk::DeferredGraphics(_)
    )
}

/// How long a completion list stays on screen after a keystroke that is about
/// to refresh it. The card only has to survive the PTY round-trip of one Tab.
pub const COMPLETION_HIDE_GRACE: std::time::Duration = std::time::Duration::from_millis(200);

#[derive(Default)]
struct CompletionSlot {
    generation: u64,
    list: Option<CompletionList>,
    /// Set when input that is expected to refresh the list was sent. The list
    /// stays visible until this passes, so a Tab that re-publishes the same
    /// candidates does not blink the card off for the round-trip.
    ///
    /// Deliberately no timer: the shell's reply lands well inside the window in
    /// the normal case and clears the deadline, and the cursor-blink tick
    /// already bounds how long a stale card can linger when a foreground
    /// program swallows the Tab instead. Do not add one.
    hide_after: Option<std::time::Instant>,
}

/// The list a renderer should draw at `now`, honoring a pending grace-hide.
fn completion_visible(slot: &CompletionSlot, now: std::time::Instant) -> Option<&CompletionList> {
    match slot.hide_after {
        Some(deadline) if now >= deadline => None,
        _ => slot.list.as_ref(),
    }
}

fn poll_completion_hide_slot(
    slot: &mut CompletionSlot,
    now: std::time::Instant,
) -> (bool, Option<std::time::Duration>) {
    let Some(deadline) = slot.hide_after else {
        return (false, None);
    };
    if now < deadline {
        return (false, Some(deadline.saturating_duration_since(now)));
    }
    slot.hide_after = None;
    (slot.list.take().is_some(), None)
}

fn apply_completion_update(cell: &Mutex<CompletionSlot>, update: CompletionUpdate) {
    let Ok(mut current) = cell.lock() else {
        return;
    };
    match update {
        CompletionUpdate::Show(list) | CompletionUpdate::Update(list)
            if list.generation >= current.generation =>
        {
            current.generation = list.generation;
            current.list = Some(list);
            // The refresh the grace window was waiting for arrived.
            current.hide_after = None;
        }
        CompletionUpdate::Clear { generation } if generation >= current.generation => {
            current.generation = generation;
            current.list = None;
            current.hide_after = None;
        }
        CompletionUpdate::Show(_)
        | CompletionUpdate::Update(_)
        | CompletionUpdate::Clear { .. } => {}
    }
}

fn reset_completion_session(cell: &Mutex<CompletionSlot>) {
    if let Ok(mut current) = cell.lock() {
        *current = CompletionSlot::default();
    }
}

#[cfg(test)]
mod completion_state_tests {
    use super::{
        COMPLETION_HIDE_GRACE, CompletionSlot, apply_completion_update, completion_visible,
        poll_completion_hide_slot, reset_completion_session,
    };
    use kettle_vt::{CompletionKind, CompletionList, CompletionUpdate};
    use std::sync::Mutex;
    use std::time::Instant;

    fn list(generation: u64) -> CompletionList {
        CompletionList {
            generation,
            kind: CompletionKind::Completion,
            selected: None,
            source: "test".to_string(),
            candidates: vec![kettle_vt::CompletionCandidate {
                label: "candidate".to_string(),
                description: String::new(),
            }],
        }
    }

    #[test]
    fn a_new_prompt_accepts_a_nested_shells_fresh_generation() {
        let slot = Mutex::new(CompletionSlot::default());
        apply_completion_update(&slot, CompletionUpdate::Show(list(42)));
        reset_completion_session(&slot);
        apply_completion_update(&slot, CompletionUpdate::Show(list(1)));

        let current = slot.lock().unwrap();
        assert_eq!(current.generation, 1);
        assert_eq!(current.list.as_ref().map(|list| list.generation), Some(1));
    }

    #[test]
    fn a_clear_keeps_older_updates_from_resurrecting() {
        let slot = Mutex::new(CompletionSlot::default());
        apply_completion_update(&slot, CompletionUpdate::Show(list(7)));
        apply_completion_update(&slot, CompletionUpdate::Clear { generation: 8 });
        apply_completion_update(&slot, CompletionUpdate::Update(list(7)));

        let slot = slot.lock().unwrap();
        assert_eq!(slot.generation, 8);
        assert!(slot.list.is_none());
    }

    #[test]
    fn a_pending_grace_window_keeps_the_card_up_until_it_lapses() {
        let now = Instant::now();
        let mut slot = CompletionSlot {
            generation: 3,
            list: Some(list(3)),
            hide_after: Some(now + COMPLETION_HIDE_GRACE),
        };
        assert!(completion_visible(&slot, now).is_some());
        assert!(
            completion_visible(&slot, now + COMPLETION_HIDE_GRACE).is_none(),
            "a Tab the shell never answered must stop showing stale candidates"
        );

        // The shell answering inside the window cancels the hide entirely.
        slot.hide_after = Some(now + COMPLETION_HIDE_GRACE);
        let cell = Mutex::new(slot);
        apply_completion_update(&cell, CompletionUpdate::Update(list(4)));
        let slot = cell.lock().unwrap();
        assert!(slot.hide_after.is_none());
        assert!(completion_visible(&slot, now + COMPLETION_HIDE_GRACE * 10).is_some());
    }

    #[test]
    fn grace_expiry_requests_exactly_one_erase_frame() {
        let now = Instant::now();
        let mut slot = CompletionSlot {
            generation: 3,
            list: Some(list(3)),
            hide_after: Some(now + COMPLETION_HIDE_GRACE),
        };
        assert_eq!(
            poll_completion_hide_slot(&mut slot, now),
            (false, Some(COMPLETION_HIDE_GRACE))
        );
        assert_eq!(
            poll_completion_hide_slot(&mut slot, now + COMPLETION_HIDE_GRACE),
            (true, None)
        );
        assert_eq!(
            poll_completion_hide_slot(&mut slot, now + COMPLETION_HIDE_GRACE * 2),
            (false, None),
            "the event loop must quiesce after the erase redraw"
        );
    }

    #[test]
    fn a_stale_update_neither_reopens_the_card_nor_extends_the_window() {
        let now = Instant::now();
        let deadline = now + COMPLETION_HIDE_GRACE;
        let cell = Mutex::new(CompletionSlot {
            generation: 9,
            list: None,
            hide_after: Some(deadline),
        });
        apply_completion_update(&cell, CompletionUpdate::Update(list(8)));

        let slot = cell.lock().unwrap();
        assert_eq!(slot.generation, 9);
        assert!(slot.list.is_none());
        assert_eq!(slot.hide_after, Some(deadline));
    }
}

fn reset_deferred_graphics(
    active_alternate: &mut bool,
    deferred: &mut DeferredGraphicsJournal,
    registries: GraphicsRegistries<'_>,
    extractor: &mut Extractor,
) {
    deferred.fail_closed();
    reset_all_graphics_to_screen(active_alternate, registries, extractor);
}

fn apply_sync_marker(
    term: &mut Term<EventProxy>,
    id: u64,
    active_alternate: &mut bool,
    deferred: &mut DeferredGraphicsJournal,
    registries: GraphicsRegistries<'_>,
    actions: GraphicsActionContext<'_>,
    extractor: &mut Extractor,
) {
    let mut batch = term.take_graphics_events();
    let marker_matches = matches!(batch.events.pop(), Some(GraphicsEvent::SyncMarker { id: marker_id }) if marker_id == id);
    if deferred.overflowed || batch.overflowed || !marker_matches {
        *active_alternate = batch.alternate_screen;
        reset_deferred_graphics(active_alternate, deferred, registries, extractor);
        return;
    }
    if !apply_graphics_batch(batch, active_alternate, registries, extractor) {
        deferred.fail_closed();
        return;
    }
    let Some(graphics) = deferred.take(id) else {
        reset_deferred_graphics(active_alternate, deferred, registries, extractor);
        return;
    };

    extractor.set_graphics_deferred(false);
    let mut replayed = Vec::new();
    extractor.feed_with(graphics.as_bytes(), |_, chunk| replayed.push(chunk));
    extractor.set_graphics_deferred(true);
    for chunk in replayed {
        match chunk {
            Chunk::Pass(_) | Chunk::Terminal(_) | Chunk::Raw(_) => {
                // The downstream text engine intentionally ignores malformed
                // or incomplete graphics controls. The extractor still had to
                // replay them to update bounded partial-upload state.
            }
            chunk => {
                if !apply_graphics_chunk_at(term, chunk, actions, extractor) {
                    reset_deferred_graphics(active_alternate, deferred, registries, extractor);
                    return;
                }
            }
        }
    }
}

enum SyncGraphicsDispatch<'a> {
    Marker(&'a mut Term<EventProxy>, u64),
    Batch(GraphicsEventBatch),
}

struct SyncGraphicsContext<'a> {
    active_alternate: &'a mut bool,
    deferred: &'a mut DeferredGraphicsJournal,
    registries: GraphicsRegistries<'a>,
    actions: GraphicsActionContext<'a>,
    extractor: &'a mut Extractor,
}

fn apply_sync_dispatch(dispatch: SyncGraphicsDispatch<'_>, context: &mut SyncGraphicsContext<'_>) {
    match dispatch {
        SyncGraphicsDispatch::Marker(term, id) => apply_sync_marker(
            term,
            id,
            context.active_alternate,
            context.deferred,
            context.registries,
            context.actions,
            context.extractor,
        ),
        SyncGraphicsDispatch::Batch(batch) => {
            if (batch.overflowed
                || !batch.events.is_empty()
                || batch.alternate_screen != *context.active_alternate)
                && !apply_graphics_batch(
                    batch,
                    context.active_alternate,
                    context.registries,
                    context.extractor,
                )
            {
                context.deferred.fail_closed();
            }
        }
    }
}

fn finish_deferred_sync(context: &mut SyncGraphicsContext<'_>) {
    context.extractor.set_graphics_deferred(false);
    if !context.deferred.finish_sync() {
        reset_all_graphics_to_screen(
            context.active_alternate,
            context.registries,
            context.extractor,
        );
    }
}

/// Advance terminal bytes and replay deferred graphics at their exact
/// synchronized-output byte offsets.
fn advance_terminal_bytes(
    processor: &mut Processor,
    term: &SharedTerm,
    bytes: &[u8],
    context: &mut SyncGraphicsContext<'_>,
) {
    advance_terminal_bytes_with_commit_hook(processor, term, bytes, context, |_| {});
}

/// Testable form of [`advance_terminal_bytes`] exposing the real boundary
/// after the text engine commits and releases its mutex but before the
/// resulting graphics journal is applied.
fn advance_terminal_bytes_with_commit_hook(
    processor: &mut Processor,
    term: &SharedTerm,
    bytes: &[u8],
    context: &mut SyncGraphicsContext<'_>,
    after_text_commit: impl FnOnce(&GraphicsEventBatch),
) {
    let batch = {
        let mut term = term.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        processor.advance_with_sync_markers(&mut *term, bytes, &mut |term, id| {
            apply_sync_marker(
                term,
                id,
                context.active_alternate,
                context.deferred,
                context.registries,
                context.actions,
                context.extractor,
            );
        });
        term.take_graphics_events()
    };
    after_text_commit(&batch);
    let sync_pending = processor.sync_timeout().sync_timeout().is_some();
    apply_sync_dispatch(SyncGraphicsDispatch::Batch(batch), context);
    if sync_pending {
        context.extractor.set_graphics_deferred(true);
    } else {
        finish_deferred_sync(context);
    }
}

/// Force-apply a pending DEC 2026 update, prune image rows evicted by the
/// buffered mutation, then publish exactly one redraw.
struct SyncFlushContext<'a> {
    term: &'a SharedTerm,
    images: &'a Images,
    graphics_gate: &'a Mutex<()>,
    image_pruner: &'a mut ImageHistoryPruner,
    on_graphics: &'a mut dyn for<'term> FnMut(SyncGraphicsDispatch<'term>),
    out_gen: &'a std::sync::atomic::AtomicU64,
    output_wake: &'a OutputWakeGate,
}

fn force_sync_update_flush(processor: &mut Processor, context: &mut SyncFlushContext<'_>) {
    let _graphics_guard = context
        .graphics_gate
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let batch = {
        let mut term = context
            .term
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        processor.stop_sync_with_markers(&mut *term, &mut |term, id| {
            (context.on_graphics)(SyncGraphicsDispatch::Marker(term, id));
        });
        term.take_graphics_events()
    };
    (context.on_graphics)(SyncGraphicsDispatch::Batch(batch));
    context
        .image_pruner
        .prune_if_changed(context.term, context.images);
    context
        .out_gen
        .fetch_add(1, std::sync::atomic::Ordering::Release);
    context.output_wake.request();
}

fn publish_output_if_ready(
    processor: &Processor,
    out_gen: &std::sync::atomic::AtomicU64,
    output_wake: &OutputWakeGate,
) -> bool {
    if processor.sync_timeout().sync_timeout().is_some() {
        return false;
    }
    out_gen.fetch_add(1, std::sync::atomic::Ordering::Release);
    output_wake.request();
    true
}

#[derive(Debug, PartialEq, Eq)]
enum PtyPumpSend {
    Forwarded,
    Drain(Vec<u8>),
    Disconnected,
}

/// Forward one bounded PTY chunk, but yield immediately to teardown draining.
///
/// A plain blocking send can strand the pump behind a full parser queue just
/// when legacy `ClosePseudoConsole` needs that pump to consume conout. The
/// short timed wait preserves bounded backpressure during normal operation
/// while making teardown observation independent of parser progress.
fn forward_pty_buffer_or_drain(
    raw_tx: &crossbeam_channel::Sender<Option<Vec<u8>>>,
    drain_output: &AtomicBool,
    mut buffer: Vec<u8>,
) -> PtyPumpSend {
    loop {
        if drain_output.load(Ordering::Acquire) {
            return PtyPumpSend::Drain(buffer);
        }

        match raw_tx.send_timeout(Some(buffer), std::time::Duration::from_millis(10)) {
            Ok(()) => return PtyPumpSend::Forwarded,
            Err(crossbeam_channel::SendTimeoutError::Timeout(Some(returned))) => {
                buffer = returned;
            }
            Err(crossbeam_channel::SendTimeoutError::Timeout(None)) => {
                unreachable!("PTY pump only forwards populated chunks");
            }
            Err(crossbeam_channel::SendTimeoutError::Disconnected(_)) => {
                return PtyPumpSend::Disconnected;
            }
        }
    }
}

/// Receive one bounded pump chunk while enforcing the DEC 2026 deadline ahead
/// of ready data, and flushing immediately when EOF makes a terminator impossible.
fn receive_pty_chunk(
    processor: &mut Processor,
    raw_rx: &crossbeam_channel::Receiver<Option<Vec<u8>>>,
    context: &mut SyncFlushContext<'_>,
) -> Option<Vec<u8>> {
    loop {
        match processor.sync_timeout().sync_timeout() {
            Some(deadline) => {
                let now = std::time::Instant::now();
                if now >= deadline {
                    force_sync_update_flush(processor, context);
                    continue;
                }

                let wait = deadline.saturating_duration_since(now);
                match raw_rx.recv_timeout(wait) {
                    Ok(chunk) => {
                        // A ready chunk and the timeout can race at the wait
                        // boundary. The expired synchronized update takes
                        // priority so sustained output cannot starve its flush.
                        // EOF also proves no closing sequence can still arrive.
                        if chunk.is_none() || std::time::Instant::now() >= deadline {
                            force_sync_update_flush(processor, context);
                        }
                        return chunk;
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                        force_sync_update_flush(processor, context);
                    }
                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                        force_sync_update_flush(processor, context);
                        return None;
                    }
                }
            }
            None => return raw_rx.recv().ok().flatten(),
        }
    }
}

/// Grid dimensions passed to `alacritty_terminal` (implements `Dimensions`).
#[derive(Clone, Copy)]
pub struct TermSize {
    pub columns: usize,
    pub screen_lines: usize,
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.screen_lines
    }
    fn screen_lines(&self) -> usize {
        self.screen_lines
    }
    fn columns(&self) -> usize {
        self.columns
    }
}

fn commit_local_geometry(
    term: &SharedTerm,
    geometry: &Arc<Mutex<VersionedPtyGeometry>>,
    desired: PtyGeometry,
    grid_options: Option<TermConfig>,
) {
    // This lock order is an invariant shared with image placement and geometry
    // snapshots: Term first, geometry second.
    let mut term = term.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut geometry = geometry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(options) = grid_options {
        term.resize(TermSize {
            columns: desired.columns,
            screen_lines: desired.rows,
        });
        term.set_options(options);
    }
    geometry.geometry = desired;
    geometry.generation = geometry.generation.wrapping_add(1);
}

#[cfg(test)]
fn local_geometry_snapshot(
    term: &SharedTerm,
    geometry: &Arc<Mutex<VersionedPtyGeometry>>,
) -> (usize, usize, PtyGeometry, u64) {
    let term = term.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let geometry = geometry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    (
        term.columns(),
        term.screen_lines(),
        geometry.geometry,
        geometry.generation,
    )
}

pub type SharedTerm = Arc<Mutex<Term<EventProxy>>>;

/// Best-effort "user home" directory for a freshly spawned shell whose
/// recorded cwd is missing or no longer on disk. Probes the platform-
/// conventional env vars in order:
/// - `HOME` — always set on Linux / macOS
/// - `USERPROFILE` — the Windows-native home (`C:\Users\Bob`)
/// - `APPDATA` — Windows last-ditch fallback (`...\AppData\Roaming`)
///
/// An *empty* env var (e.g., `HOME=""` — possible in stripped-down CI
/// containers or after a misconfigured shell `unset HOME` / `export
/// HOME=`) is treated as unset and the probe continues to the next
/// variable. Previously, `var_os("HOME")` would return
/// `Some(OsString::new())` and this function returned `PathBuf::from("")`
/// — `CommandBuilder::cwd("")` then fed an invalid empty path to the
/// OS spawn call (which on Unix means "no cwd" but the intent here is
/// to actively *pick* a home, so the silent fall-through was wrong).
///
/// Returns `None` only on a stripped-down environment where none of
/// the three are set to a non-empty value; callers leave
/// `CommandBuilder::cwd` unset in that case, which makes `portable_pty`
/// inherit kettle's launch directory.
///
/// `lookup` is passed in so the env-probe order is unit-testable
/// without touching the process env (which would race with parallel
/// tests). Production code calls with `|k| std::env::var_os(k)`.
pub(crate) fn home_dir_fallback(
    lookup: impl Fn(&str) -> Option<std::ffi::OsString>,
) -> Option<std::path::PathBuf> {
    let pick = |k: &str| lookup(k).filter(|v| !v.is_empty());
    pick("HOME")
        .or_else(|| pick("USERPROFILE"))
        .or_else(|| pick("APPDATA"))
        .map(std::path::PathBuf::from)
}

/// Terminator parity (`command_notify.py` plugin): a single
/// completed-command event for the App's notification dispatcher. Built
/// from the OSC 133 `OutputStart` → `CommandEnd` transition; the App
/// uses `duration` + window focus to decide whether to fire a desktop
/// notification. `exit_code` is the OSC 133 D payload (`None` when the
/// shell didn't ship one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandFinished {
    pub duration: std::time::Duration,
    pub exit_code: Option<i32>,
}

/// A protocol-requested desktop notification from the PTY. Produced by
/// `OSC 9 ; message` or `OSC 777 ; notify ; title ; body` after the extractor
/// validates and caps each field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolNotification {
    pub title: String,
    pub body: String,
}

/// Why a per-pane session log stopped before the user toggled it off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionLogFailure {
    Overloaded,
    IoError,
}

/// Open the private append target only after the persistence worker owns this
/// value. Creating or hardening a file can reach a slow filesystem just as a
/// data write can, so winit must not perform that preparation for the parser.
struct LazySessionLogWriter {
    path: std::path::PathBuf,
    writer: Option<std::io::BufWriter<std::fs::File>>,
}

impl LazySessionLogWriter {
    fn new(path: std::path::PathBuf) -> Self {
        Self { path, writer: None }
    }

    fn writer(&mut self) -> std::io::Result<&mut std::io::BufWriter<std::fs::File>> {
        if self.writer.is_none() {
            let file = kettle_state::open_private_file_append(&self.path)?;
            self.writer = Some(std::io::BufWriter::new(file));
        }
        Ok(self
            .writer
            .as_mut()
            .expect("session log writer was initialized above"))
    }
}

impl Write for LazySessionLogWriter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.writer()?.write(bytes)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer()?.flush()
    }
}

#[cfg(windows)]
#[derive(Default)]
struct ConPtyCloseState {
    completed: AtomicBool,
    drop_requested: AtomicBool,
}

#[cfg(windows)]
impl ConPtyCloseState {
    /// Called by the sole owner of the ConPTY master after
    /// `ClosePseudoConsole` has really returned.
    fn close_completed(&self, stop: &AtomicBool) {
        self.completed.store(true, Ordering::Release);
        if self.drop_requested.load(Ordering::Acquire) {
            stop.store(true, Ordering::Release);
        }
    }

    /// Transfer reader-stop ownership to the asynchronous close worker. If it
    /// already finished, this side publishes the stop itself; otherwise the
    /// worker observes the request after its blocking close returns.
    fn terminal_dropped(&self, stop: &AtomicBool) {
        self.drop_requested.store(true, Ordering::Release);
        if self.completed.load(Ordering::Acquire) {
            stop.store(true, Ordering::Release);
        }
    }
}

pub struct Terminal {
    pub term: SharedTerm,
    term_config: TermConfig,
    scrollback_line_limit: usize,
    scrollback_byte_limit: usize,
    // `Option` so `Drop` can `.take()` and drop the master
    // (ClosePseudoConsole on Windows / close the master fd on Unix) WITHOUT
    // moving a non-`Option` field out of `&mut self`. Always `Some` during
    // normal operation; only `None` transiently inside `Drop`.
    master: Option<Box<dyn portable_pty::MasterPty + Send>>,
    /// State shared with an asynchronous ConPTY close. Once the master moves
    /// to that worker, only it may publish `stop`: on older Windows releases
    /// `ClosePseudoConsole` can wait for the reader to drain conout, so a later
    /// `Terminal::drop` must not stop that reader first.
    #[cfg(windows)]
    pty_close: Option<Arc<ConPtyCloseState>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// Unix `O_NONBLOCK` is status on the shared PTY master open-file
    /// description, not on one duplicated descriptor. Permit exactly one
    /// `PtyStdin` lease so an older handle can never restore blocking mode
    /// underneath a newer live handle.
    #[cfg(unix)]
    stdin_lease_phase: Arc<Mutex<PtyStdinLeasePhase>>,
    /// Immutable process id captured before the child handle is shared with
    /// the teardown reaper. Input classification must never take `child`: the
    /// reaper deliberately holds that mutex across a blocking `wait()`.
    child_pid: Option<u32>,
    child: Arc<Mutex<Box<dyn portable_pty::Child + Send + Sync>>>,
    reader_thread: Option<JoinHandle<()>>,
    /// Published before the reader's sole raw-output sender is dropped, so a
    /// headless consumer can tell orderly EOF from an unexpected read error.
    /// Status, source generation, and pending parser work in one atomic word so
    /// headless completion cannot observe a combination that never existed.
    pty_read_progress: Arc<PtyReadProgressState>,
    /// Direct-child exit observed without consuming its status. PTY EOF can
    /// legitimately follow later; the timestamp starts the bounded drain only
    /// when a descendant keeps the transport open indefinitely.
    direct_child_exit_at: Arc<Mutex<Option<std::time::Instant>>>,
    /// Edge published for semantic terminal events and direct-child exit.
    /// Unlike output generation, this remains meaningful for quiet and hidden
    /// panes.
    lifecycle_pending: Arc<AtomicBool>,
    // Cooperative stop flag for the reader thread. The detached teardown
    // worker sets it only after closing the PTY master. Keeping it false while
    // `ClosePseudoConsole` runs is required on Windows before 11 24H2, where
    // that call can wait for the output pipe to be drained.
    stop: Arc<AtomicBool>,
    // Set by `Drop` before the detached close starts. It interrupts a pump
    // blocked on the bounded parser queue and switches that pump to direct
    // discard/drain mode until the platform close returns.
    drain_output: Arc<AtomicBool>,
    pub cols: usize,
    pub rows: usize,
    pub images: Images,
    /// kitty `U=1` virtual images, keyed by image id (for placeholder draw).
    pub virtuals: Virtuals,
    /// kitty animations, keyed by image id (frame substituted at draw time).
    pub anims: Animations,
    /// kitty relative placements, keyed by `(child img, child placement)`.
    pub relatives: Relatives,
    /// The primary screen's graphics while the alternate screen is active.
    /// Active registries above are swapped as a unit at buffer transitions.
    inactive_graphics: InactiveGraphics,
    /// Serializes screen-buffer swaps and column-reflow invalidation against
    /// graphics chunks decoded by the PTY reader.
    graphics_gate: Arc<Mutex<()>>,
    /// Resize-side publication that tells the reader's Kitty decoder to drop
    /// regular/relative placement anchors after a column reflow.
    graphics_reflow_generation: Arc<std::sync::atomic::AtomicU64>,
    /// Monotonic document-row ids where OSC 133 prompts started.
    ///
    /// These combine alacritty's grid `history_origin` with its current
    /// history-relative cursor line. Unlike plain `history_size + line`, they
    /// are never reused when a full scrollback ring evicts its oldest row.
    /// A `VecDeque` so the ring-buffer trim is an O(1)
    /// `pop_front`, not a `Vec::drain(0..1)` that shifts all ~2048 elements on
    /// every prompt once full — this is the hot reader-thread path.
    prompts: Arc<Mutex<std::collections::VecDeque<u64>>>,
    /// Terminator parity (`command_notify.py` plugin):
    /// OSC 133 OutputStart timestamp — set when `OutputStart` fires,
    /// cleared when the matching `CommandEnd` fires. The reader
    /// thread updates this; the App reads `command_finished` (below)
    /// to learn that a command completed + how long it took.
    pub output_started_at: Arc<Mutex<Option<std::time::Instant>>>,
    /// Per-pane queue of completed-command events.
    /// Populated by the reader thread on OSC 133 D (CommandEnd) when
    /// `output_started_at` is `Some`; drained by the App each tick
    /// to fire desktop notifications (if the window isn't focused
    /// and the command ran longer than `cfg.command_notify_threshold_ms`).
    /// Bounded at 32 entries — a hostile / runaway script that
    /// emitted thousands of fake OSC 133 D sequences would otherwise
    /// grow this Vec indefinitely.
    pub command_finished: Arc<Mutex<Vec<CommandFinished>>>,
    /// Protocol desktop notifications requested by the PTY. Bounded in the
    /// reader thread so a hostile program cannot queue unbounded toasts.
    pub protocol_notifications: Arc<Mutex<Vec<ProtocolNotification>>>,
    /// Latest shell-owned completion list. The extractor bounds and validates
    /// it before publication; renderers clone at most 64 short rows.
    completion: Arc<Mutex<CompletionSlot>>,
    /// Latest working directory reported via OSC 7 (or OSC 9;9). This is the
    /// *authoritative* cwd — a shell that volunteers it (incl. an in-distro WSL
    /// shell) is always right.
    pub cwd: Arc<Mutex<Option<String>>>,
    /// v2.29.0: a working directory read natively from the OS (the PTY child's
    /// foreground process, via the platform process table) when the shell does
    /// NOT emit OSC 7/9;9 — e.g. a stock Windows `pwsh`/`cmd`. Kept SEPARATE
    /// from `cwd` so a stale/None native read can never clobber the authoritative
    /// escape-sequence cwd; consulted only as a fallback by `current_dir_or_native`.
    /// Never set for WSL/SSH panes (the relay's OS cwd is meaningless there).
    pub native_cwd: Arc<Mutex<Option<String>>>,
    /// v2.29.1: set once the shell actually reported a cwd via OSC 7/9;9. Until
    /// then `cwd` holds only the pre-seeded launch directory, so
    /// [`current_dir_or_native`](Self::current_dir_or_native) prefers the live
    /// native poll; after a real report the reported cwd becomes authoritative.
    pub osc_cwd_seen: Arc<std::sync::atomic::AtomicBool>,
    /// Latest OSC 9;4 progress state (drives the OS taskbar
    /// indicator); `None` until the program reports progress / after clear.
    pub progress: Arc<Mutex<Option<Progress>>>,
    /// The argv this pane was launched with (empty = default shell);
    /// persisted so SSH/remote panes can be restored.
    pub argv: Vec<String>,
    /// Terminator parity (`plugins/logger.py`): optional per-pane session log.
    /// The reader admits raw PTY bytes to a bounded worker and never performs
    /// filesystem I/O. When `None`, zero cost on the hot path — the `log_active`
    /// flag short-circuits before the lock.
    /// Private: flip it via [`Terminal::set_log_path`] or
    /// [`Terminal::set_log_file`]
    /// so `log_active` can never drift out of sync.
    session_log: Arc<Mutex<Option<AsyncFileWriter>>>,
    /// When `true`, the logger strips ANSI escape
    /// sequences (CSI / OSC / single-char ESC) from the bytes
    /// before writing — leaving plain-text-searchable logs.
    /// Default `false` preserves the raw-stream
    /// behavior (replayable via `cat <log>` in a terminal).
    pub log_strip_ansi: Arc<Mutex<bool>>,
    /// v2.20.0 (`shell_idle`, review fix): true once this pane has seen at
    /// least one OSC 133 OutputStart (C). An integration that emits A/B/D
    /// but never C (a clobbered pwsh Enter handler, AcceptLine via other
    /// chords) must never report "idle" — prompt marks alone don't prove
    /// command tracking works, and a false idle skips the close-confirm
    /// dialog over a running command.
    output_start_seen: Arc<std::sync::atomic::AtomicBool>,
    /// Mirrors an active session-log writer so the reader thread can skip its
    /// per-read Mutex entirely when logging is off
    /// (the overwhelmingly common case — the lock + Option check ran once
    /// per 64KiB read). Toggled ONLY through [`Terminal::set_log_path`] or
    /// [`Terminal::set_log_file`],
    /// which keeps the pair in sync.
    log_active: Arc<std::sync::atomic::AtomicBool>,
    /// Changes whenever the installed writer's logging session changes. The
    /// reader uses it to keep parser state from crossing session boundaries.
    log_generation: Arc<std::sync::atomic::AtomicU64>,
    /// Keeps worker-side failure reporting edge-triggered while preserving the
    /// failed writer long enough for the UI to observe its exact reason.
    log_failure_reported: AtomicBool,
    /// A persistence failure can happen after PTY output falls silent, so the
    /// writer needs an independent route back to the event loop.
    log_waker: Waker,
    /// Exact grid + pixel geometry shared with the PTY reader's image parser.
    /// Image-to-cell conversion divides by the effective fractional cell size
    /// derived from these totals, so it agrees with both `PtySize` and CSI 14t.
    geometry: Arc<Mutex<VersionedPtyGeometry>>,
    /// Last geometry successfully published to the native PTY. This is kept
    /// separate from the desired/local geometry: if `MasterPty::resize` fails,
    /// the next identical UI resize still retries instead of being suppressed
    /// by the already-updated terminal grid.
    applied_pty_geometry: PtyGeometry,
    /// Per-pane output wake publication gate. Paint scheduling stays quiescent
    /// while a window is hidden/minimized, but the UI deliberately leaves this
    /// transport wake enabled when a recorder or Lua output sidechannel must
    /// drain its bounded queue. A hidden pane without either consumer
    /// coalesces output into one dirty bit and publishes one wake when enabled.
    output_wake: Arc<OutputWakeGate>,
    /// A bounded semantic-event queue overflow cannot block the parser while it
    /// holds the terminal lock. It instead marks the pane failed so the UI can
    /// tear it down explicitly without unbounded memory or silent reply loss.
    event_overflowed: Arc<AtomicBool>,
    /// Live OSC 52 write policy shared with the terminal event proxy. DA1
    /// extension 52 is emitted only while this is true.
    osc52_copy_allowed: Arc<AtomicBool>,
    /// C4 (multi-window): bumped by the reader thread once per PTY read it
    /// processed (right before the wakeup fires). Lets a UI hosting several
    /// windows answer "did THIS pane produce output since I last painted?"
    /// without draining anything — a fan-out wakeup repaints only the windows
    /// whose panes' generations moved. Plain text emits no `TermEvent`, so
    /// the event channel can't answer that question.
    out_gen: Arc<std::sync::atomic::AtomicU64>,
}

/// Command state reported by OSC 133 shell integration.
///
/// `Unknown` is deliberately distinct from `Running`: without a complete
/// integration Kettle must not infer either that a shell is idle or that a
/// modified-key fallback is safe to send.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShellActivity {
    Unknown,
    Idle,
    Running,
}

fn classify_shell_activity(
    seen_prompts: bool,
    tracks_commands: bool,
    running: Option<bool>,
) -> ShellActivity {
    if !seen_prompts || !tracks_commands {
        return ShellActivity::Unknown;
    }
    match running {
        Some(true) => ShellActivity::Running,
        Some(false) => ShellActivity::Idle,
        None => ShellActivity::Unknown,
    }
}

#[cfg(test)]
mod shell_activity_tests {
    use super::{ShellActivity, classify_shell_activity};

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
}

#[cfg(all(test, unix))]
mod foreground_job_tests {
    use super::*;

    fn wait_for_screen(
        terminal: &Terminal,
        events: &crossbeam_channel::Receiver<TermEvent>,
        needle: &str,
    ) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            while let Ok(event) = events.try_recv() {
                if let TermEvent::PtyWrite(reply) = event {
                    terminal.write(reply.as_bytes());
                }
            }
            if terminal
                .screen_text(0)
                .is_some_and(|screen| screen.text.contains(needle))
            {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    fn wait_for_job_state(
        terminal: &Terminal,
        events: &crossbeam_channel::Receiver<TermEvent>,
        foreground_job: bool,
    ) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            while let Ok(event) = events.try_recv() {
                if let TermEvent::PtyWrite(reply) = event {
                    terminal.write(reply.as_bytes());
                }
            }
            let observed = terminal
                .foreground_process_group()
                .ok()
                .and_then(|foreground| {
                    let child_pid = terminal.child_pid()? as libc::pid_t;
                    let child_group = unsafe { libc::getpgid(child_pid) };
                    (child_group >= 0).then_some(foreground != child_group as u32)
                });
            if matches!(terminal.input_is_canonical(), Ok(false))
                && observed == Some(foreground_job)
            {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    /// The exact Unix regression: zsh ZLE and Bash Readline are noncanonical at
    /// their prompts, so termios alone classifies either as a TUI and leaks
    /// `;2;13~`. Foreground job control is what distinguishes the shell's own
    /// editor from the raw command it launches.
    #[test]
    fn raw_shell_editor_is_not_a_foreground_job_but_a_raw_child_is() {
        #[cfg(target_os = "macos")]
        let argv = vec!["/bin/zsh".to_string(), "-f".to_string()];
        #[cfg(not(target_os = "macos"))]
        let argv = vec![
            "/bin/bash".to_string(),
            "--noprofile".to_string(),
            "--norc".to_string(),
        ];
        let (tx, rx) = crossbeam_channel::unbounded();
        let waker: Waker = Arc::new(|| {});
        let terminal = match Terminal::new(
            &argv,
            None,
            100,
            80,
            24,
            8,
            16,
            false,
            CursorShape::Block,
            None,
            tx,
            waker,
        ) {
            Ok(terminal) => terminal,
            Err(error) => {
                eprintln!("skipping shell job-control test: no PTY ({error})");
                return;
            }
        };

        // Split the marker in the command text so the shell's own echo cannot
        // satisfy `wait_for_screen` before `printf` actually runs.
        terminal.write(b"printf 'SHELL_PROMPT''_READY\\n'\r");
        assert!(wait_for_screen(&terminal, &rx, "SHELL_PROMPT_READY"));
        assert!(
            wait_for_job_state(&terminal, &rx, false),
            "the shell line editor did not regain a raw foreground prompt"
        );

        terminal.write(
            b"/bin/sh -c 'stty raw -echo; printf RAW_JOB''_READY; dd bs=1 count=1 >/dev/null 2>&1; stty sane'\r",
        );
        assert!(wait_for_screen(&terminal, &rx, "RAW_JOB_READY"));
        assert!(
            wait_for_job_state(&terminal, &rx, true),
            "the raw child did not take the PTY foreground process group"
        );
        terminal.write(b"x");
    }
}

/// Pick Windows' default shell when the user configured no
/// `command` / `shell`. Prefers PowerShell 7+ (`pwsh.exe`), then Windows
/// PowerShell 5.1 (`powershell.exe`); returns `None` to let the caller fall
/// back to portable_pty's default (`%ComSpec%` → `cmd.exe`). This matches
/// Windows Terminal, which defaults to pwsh 7 when it is installed — a plain
/// `cmd.exe` default feels dated on a modern Windows 11 box. `resolve` maps
/// an exe name to its full path if present on `PATH`; it is injected so the
/// preference order is unit-testable without depending on what is installed.
#[cfg(windows)]
fn pick_windows_default_shell(
    resolve: impl Fn(&str) -> Option<std::path::PathBuf>,
) -> Option<std::path::PathBuf> {
    ["pwsh.exe", "powershell.exe"].into_iter().find_map(resolve)
}

/// Full path of `exe` if it is a file on any `PATH` entry.
/// pwsh 7's installer adds `C:\Program Files\PowerShell\7` to `PATH`, and
/// `powershell.exe` lives in `System32` (always on `PATH`), so a bare-name
/// PATH walk resolves both without hard-coding install locations.
#[cfg(windows)]
fn find_on_path(exe: &str) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(exe))
        .find(|p| {
            // NOT `is_file()`: that follows reparse points and FAILS on the
            // Store "app execution alias" stubs (0-byte reparse points under
            // `%LOCALAPPDATA%\Microsoft\WindowsApps\`) that a Store-installed
            // pwsh 7 uses. `symlink_metadata` (lstat) succeeds on the alias
            // itself, so it detects both a real `pwsh.exe` and a Store alias;
            // exclude directories so a stray dir named `pwsh.exe` can't match.
            std::fs::symlink_metadata(p)
                .map(|m| !m.is_dir())
                .unwrap_or(false)
        })
}

/// `portable-pty` refreshes Windows' system/user registry environment after it
/// snapshots the current process. That is useful for desktop launches, but it
/// overwrites session-local values inherited from a shell (virtualenvs, package
/// manager shims, and temporary PATH prefixes). Restore the actual parent
/// environment, then append registry-only PATH entries behind the parent's
/// ordering. Explicit Kettle config env is applied after this helper.
#[cfg(windows)]
fn overlay_windows_parent_env(
    cmd: &mut CommandBuilder,
    parent: impl IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
) {
    let registry_path = cmd.get_env("PATH").map(std::ffi::OsStr::to_os_string);
    let mut parent_path = None;
    for (name, value) in parent {
        if name.to_string_lossy().eq_ignore_ascii_case("PATH") {
            parent_path = Some(value.clone());
        }
        cmd.env(name, value);
    }
    if let Some(path) = merge_windows_paths(parent_path.as_deref(), registry_path.as_deref()) {
        cmd.env("PATH", path);
    }
}

#[cfg(windows)]
fn merge_windows_paths(
    parent: Option<&std::ffi::OsStr>,
    registry: Option<&std::ffi::OsStr>,
) -> Option<std::ffi::OsString> {
    use std::collections::HashSet;

    let fallback = parent.or(registry).map(std::ffi::OsStr::to_os_string);
    let mut seen = HashSet::new();
    let mut entries = Vec::new();
    for path in parent
        .into_iter()
        .chain(registry)
        .flat_map(std::env::split_paths)
    {
        let mut key = path.to_string_lossy().replace('/', "\\").to_lowercase();
        while key.len() > 3 && key.ends_with('\\') {
            key.pop();
        }
        if !key.is_empty() && seen.insert(key) {
            entries.push(path);
        }
    }
    if entries.is_empty() {
        fallback
    } else {
        std::env::join_paths(entries).ok().or(fallback)
    }
}

/// Terminates a freshly spawned child unless terminal construction completes.
///
/// Reader and writer setup now finishes before `spawn_command`, so a setup
/// error cannot start a child at all. Dropping a `Box<dyn Child>` still does not
/// terminate the process it represents, however, so the guard covers the final
/// ownership handoff and protects an unwind while the value is assembled.
///
/// `Terminal`'s own `Drop` takes over once construction succeeds, so the guard
/// is disarmed immediately before the value is built — the covered window is
/// exactly the one where nothing else would have terminated the child.
struct SpawnedChildGuard {
    /// `None` once disarmed. A killer rather than the `Child` itself, so the
    /// child can move into the constructed `Terminal` untouched.
    killer: Option<Box<dyn portable_pty::ChildKiller + Send + Sync>>,
}

impl SpawnedChildGuard {
    fn arm(killer: Box<dyn portable_pty::ChildKiller + Send + Sync>) -> Self {
        Self {
            killer: Some(killer),
        }
    }

    fn disarm(&mut self) {
        self.killer = None;
    }
}

impl Drop for SpawnedChildGuard {
    fn drop(&mut self) {
        let Some(mut killer) = self.killer.take() else {
            return;
        };
        if let Err(error) = killer.kill() {
            // Construction is already failing, so there is nothing to escalate
            // to — but a child that outlives the terminal that started it must
            // never be silent.
            log::warn!(
                "terminal setup failed after the child started, and the child \
                 could not be terminated: {error}"
            );
        }
    }
}

/// Is `prog` the WSL launcher (`wsl` / `wsl.exe`, possibly given as
/// a full path)? The `login_shell` flag prepends `-l` for POSIX
/// `bash -l` login-shell semantics — but `wsl.exe -l` means **list
/// distributions**: it would print the distro list and exit instead of opening
/// an interactive shell. So the `-l` injection is suppressed for wsl. A user
/// who wants a WSL *login* shell should request it inside the distro (e.g.
/// `command = wsl.exe -d Ubuntu -- bash -l`), where `-l` reaches bash, not wsl.
/// Case-insensitive so `wsl`, `wsl.exe`, and `C:\…\wsl.exe` all match.
///
/// Splits on BOTH `/` and `\` rather than using `std::path::Path::file_stem`,
/// because `Path` only treats `\` as a separator on Windows targets — on a
/// Linux/macOS build (incl. CI) `C:\Windows\System32\wsl.exe` would be one
/// opaque component and the stem check would miss it. wsl.exe only runs on
/// Windows, but a target-independent check keeps the function and its unit
/// test correct everywhere (the cross-platform CI pretest caught the
/// `Path`-based version).
fn is_wsl_launcher(prog: &str) -> bool {
    let last = prog.rsplit(['/', '\\']).next().unwrap_or(prog);
    last.eq_ignore_ascii_case("wsl") || last.eq_ignore_ascii_case("wsl.exe")
}

/// Whether the platform's default shell (`default_prog`) accepts the POSIX `-l`
/// login switch. `false` on Windows, where `default_prog`
/// resolves to pwsh/powershell/cmd — none of which treat `-l` as a login flag —
/// so `login-shell = true` must not inject it there. `true` everywhere else,
/// where the default shell is a POSIX shell that honors `-l`.
const fn default_shell_accepts_login_flag() -> bool {
    cfg!(not(windows))
}

/// Whether an EXPLICIT `command = <prog>` accepts the POSIX `-l` login switch.
///
/// The `default_shell_accepts_login_flag` guard only covered the no-argv default-shell
/// arm; the explicit-argv arm still injected `-l` for `wsl.exe` (where `-l`
/// means "list distros") only via `!is_wsl_launcher`, leaving Windows-native
/// shells (`pwsh`/`powershell`/`cmd`) to receive a `-l` they reject. Exclude
/// both, matching on the case-insensitive basename sans `.exe`. POSIX shells
/// (bash/zsh/fish/…) and anything else honor `-l`.
fn prog_accepts_login_flag(prog: &str) -> bool {
    if is_wsl_launcher(prog) {
        return false;
    }
    let base = prog
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(prog)
        .to_ascii_lowercase();
    let base = base.strip_suffix(".exe").unwrap_or(&base);
    !matches!(base, "pwsh" | "powershell" | "cmd")
}

/// Build the `WSLENV` value that propagates kettle's
/// terminal-identity env vars into a WSL distro. WSL only forwards Windows
/// env vars listed in `WSLENV`; each is suffixed `/u` ("pass Windows→WSL
/// only"). Preserves `existing` (the user's own WSLENV) verbatim and skips
/// any `var` already present (matching on the name before any `/flags`), so
/// re-launches don't accumulate duplicates. Pure — unit-tested.
fn augment_wslenv(existing: &str, vars: &[&str]) -> String {
    let mut out = existing.to_string();
    for &var in vars {
        let present = out.split(':').any(|e| e.split('/').next() == Some(var));
        if !present {
            if !out.is_empty() {
                out.push(':');
            }
            out.push_str(var);
            out.push_str("/u");
        }
    }
    out
}

fn child_wslenv(parent: &str, extra_env: &[(String, String)]) -> String {
    let wslenv_base = extra_env
        .iter()
        .rev()
        .find(|(name, _)| name.eq_ignore_ascii_case("WSLENV"))
        .map(|(_, value)| value.as_str())
        .unwrap_or(parent);
    let mut wslenv_vars: Vec<&str> = extra_env
        .iter()
        .filter_map(|(name, _)| {
            if name.eq_ignore_ascii_case("WSLENV") {
                None
            } else {
                Some(name.as_str())
            }
        })
        .collect();
    wslenv_vars.extend(["COLORTERM", "TERM_PROGRAM", "TERM_PROGRAM_VERSION"]);
    augment_wslenv(wslenv_base, &wslenv_vars)
}

/// A shell choice for the new-tab `▾` dropdown — a display label and
/// the argv to spawn for it.
pub type ShellChoice = (String, Vec<String>);

/// Auto-detect the shells to offer in
/// the new-tab dropdown, Windows-Terminal style (and in WT's order). Always
/// returns at least one entry.
/// - Windows: PowerShell (pwsh 7), Windows PowerShell, Command Prompt (each
///   only when found on `PATH`), one entry per installed WSL distro, the
///   VS 2022 Developer Command Prompt / Developer PowerShell (via vswhere),
///   and Git Bash (registry / well-known paths / derived from git.exe).
/// - Other platforms: `$SHELL` first, then bash/zsh/fish found on `PATH`
///   (de-duped by basename).
///
/// Process-wide `OnceLock` cache: the probes spawn bounded externals
/// (`wsl.exe -l -q`, `vswhere.exe` — 2s timeout each), so they run at most
/// once per session. `prewarm_shell_detection` lets the App pay that cost on
/// a background thread at startup instead of the first dropdown open or
/// `Ctrl+Shift+N` press. Detection is injected into the inner helpers so
/// they are unit-testable without depending on what is installed.
pub fn detect_shells() -> Vec<ShellChoice> {
    static CACHE: std::sync::OnceLock<Vec<ShellChoice>> = std::sync::OnceLock::new();
    CACHE
        .get_or_init(|| {
            #[cfg(windows)]
            {
                detect_shells_windows(
                    |e| find_on_path(e).is_some(),
                    list_wsl_distros,
                    vs_dev_info,
                    git_bash_path,
                )
            }
            #[cfg(not(windows))]
            {
                detect_shells_unix(std::env::var("SHELL").ok(), unix_on_path)
            }
        })
        .clone()
}

/// Dropdown parity: warm the `detect_shells` cache off the UI thread
/// (the probes can block up to ~4s worst-case on a wedged WSL service + a
/// slow vswhere). Spawned once from `App::resumed`.
pub fn prewarm_shell_detection() {
    let _ = std::thread::Builder::new()
        .name("kettle-shell-probe".into())
        .spawn(|| {
            let _ = detect_shells();
        });
}

#[cfg(any(windows, test))]
fn detect_shells_windows(
    available: impl Fn(&str) -> bool,
    distros: impl Fn() -> Vec<String>,
    vs: impl Fn() -> Option<VsDevInfo>,
    git_bash: impl Fn() -> Option<std::path::PathBuf>,
) -> Vec<ShellChoice> {
    let mut out: Vec<ShellChoice> = Vec::new();
    // Dropdown parity: Windows Terminal's order (and its "PowerShell"
    // label for pwsh 7 — was "PowerShell 7"). The order matters beyond looks:
    // `Ctrl+Shift+N` opens the Nth entry, matching WT's profile shortcuts.
    for (label, exe) in [
        ("PowerShell", "pwsh.exe"),
        ("Windows PowerShell", "powershell.exe"),
        ("Command Prompt", "cmd.exe"),
    ] {
        if available(exe) {
            out.push((label.to_string(), vec![exe.to_string()]));
        }
    }
    if available("wsl.exe") {
        for d in distros() {
            out.push((
                format!("WSL: {d}"),
                vec!["wsl.exe".to_string(), "-d".to_string(), d],
            ));
        }
    }
    // VS 2022 Developer shells (WT auto-generates these when VS is present).
    if let Some(info) = vs() {
        let year = vs_year_from_install_path(&info.install_path).unwrap_or("2022");
        if info.has_dev_cmd_bat {
            out.push((
                format!("Developer Command Prompt for VS {year}"),
                vs_dev_cmd_argv(&info.install_path),
            ));
        }
        if info.has_dev_shell_dll {
            let host = if available("pwsh.exe") {
                "pwsh.exe"
            } else {
                "powershell.exe"
            };
            out.push((
                format!("Developer PowerShell for VS {year}"),
                vs_dev_powershell_argv(host, &info.install_path),
            ));
        }
    }
    if let Some(bash) = git_bash() {
        out.push((
            "Git Bash".to_string(),
            vec![
                bash.to_string_lossy().into_owned(),
                "-i".to_string(),
                "-l".to_string(),
            ],
        ));
    }
    // Never hand back an empty menu — the `▾` click must always do something.
    if out.is_empty() {
        out.push(("Command Prompt".to_string(), vec!["cmd.exe".to_string()]));
    }
    out
}

/// Dropdown parity: what the vswhere probe learned about the newest
/// Visual Studio. Built by the impure `vs_dev_info`, consumed by the pure
/// `detect_shells_windows` (test-injectable).
#[cfg(any(windows, test))]
#[derive(Clone, Debug, PartialEq)]
struct VsDevInfo {
    /// e.g. `C:\Program Files\Microsoft Visual Studio\2022\Community`.
    install_path: String,
    /// `Common7\Tools\VsDevCmd.bat` exists → offer the Developer Command
    /// Prompt.
    has_dev_cmd_bat: bool,
    /// `Common7\Tools\Microsoft.VisualStudio.DevShell.dll` exists → offer
    /// the Developer PowerShell.
    has_dev_shell_dll: bool,
}

/// Pure: the first non-empty trimmed line of vswhere's `-property
/// installationPath` output (it prints one path, but be lenient).
#[cfg(any(windows, test))]
fn parse_vs_install_path(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_string)
}

/// Pure: the year segment of a VS install path
/// (`...\Microsoft Visual Studio\2022\Community` → `"2022"`), so the
/// label stays truthful for a future VS without pinning a version range.
#[cfg(any(windows, test))]
fn vs_year_from_install_path(path: &str) -> Option<&str> {
    path.rsplit(['/', '\\'])
        .find(|seg| seg.len() == 4 && seg.chars().all(|c| c.is_ascii_digit()))
}

/// Pure: the Developer Command Prompt argv — `cmd.exe /k <VsDevCmd.bat>`
/// (portable-pty quotes the spaced path when building the Win32 command
/// line). Matches the shortcut VS itself installs.
#[cfg(any(windows, test))]
fn vs_dev_cmd_argv(install_path: &str) -> Vec<String> {
    vec![
        "cmd.exe".to_string(),
        "/k".to_string(),
        format!("{install_path}\\Common7\\Tools\\VsDevCmd.bat"),
    ]
}

/// Pure: the Developer PowerShell argv — import the DevShell module and
/// enter the dev environment, staying in the current directory
/// (`-SkipAutomaticLocation`, like Windows Terminal's generated profile;
/// the `-VsInstallPath` form avoids the second vswhere/JSON round-trip the
/// instanceId form needs). Single quotes in the path are doubled (the
/// PowerShell single-quote escape).
#[cfg(any(windows, test))]
fn vs_dev_powershell_argv(host: &str, install_path: &str) -> Vec<String> {
    let q = install_path.replace('\'', "''");
    vec![
        host.to_string(),
        "-NoExit".to_string(),
        "-Command".to_string(),
        format!(
            "&{{ Import-Module '{q}\\Common7\\Tools\\Microsoft.VisualStudio.DevShell.dll'; \
             Enter-VsDevShell -VsInstallPath '{q}' -SkipAutomaticLocation }}"
        ),
    ]
}

/// Dropdown parity: probe the newest VS via
/// `%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe`
/// (a fixed location Microsoft documents as stable). Worker thread + 2s
/// timeout, same shape as `list_wsl_distros` — a hung probe must not freeze
/// the UI thread (the `detect_shells` cache means this runs once).
#[cfg(windows)]
fn vs_dev_info() -> Option<VsDevInfo> {
    let pf86 = std::env::var_os("ProgramFiles(x86)")?;
    let vswhere = std::path::PathBuf::from(pf86)
        .join("Microsoft Visual Studio")
        .join("Installer")
        .join("vswhere.exe");
    if !std::fs::symlink_metadata(&vswhere)
        .map(|m| !m.is_dir())
        .unwrap_or(false)
    {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(
            std::process::Command::new(vswhere)
                .args([
                    "-latest",
                    "-products",
                    "*",
                    "-requires",
                    "Microsoft.VisualStudio.Component.CoreEditor",
                    "-property",
                    "installationPath",
                ])
                .output(),
        );
    });
    let Ok(Ok(out)) = rx.recv_timeout(std::time::Duration::from_secs(2)) else {
        return None;
    };
    if !out.status.success() {
        return None;
    }
    let install_path = parse_vs_install_path(&String::from_utf8_lossy(&out.stdout))?;
    let tools = std::path::Path::new(&install_path)
        .join("Common7")
        .join("Tools");
    let exists = |p: &std::path::Path| {
        std::fs::symlink_metadata(p)
            .map(|m| !m.is_dir())
            .unwrap_or(false)
    };
    Some(VsDevInfo {
        has_dev_cmd_bat: exists(&tools.join("VsDevCmd.bat")),
        has_dev_shell_dll: exists(&tools.join("Microsoft.VisualStudio.DevShell.dll")),
        install_path,
    })
}

/// Pure: candidate `bash.exe` locations for a found `git.exe` —
/// `<root>\cmd\git.exe` and `<root>\mingw64\bin\git.exe` both map to
/// `<root>\bin\bash.exe`; a flat `<dir>\git.exe` maps to
/// `<dir>\bash.exe`. Testable without a filesystem — but Windows-only,
/// since `Path` treats `\` as a separator only on Windows targets.
#[cfg(windows)]
fn git_bash_from_git_exe(git_exe: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Some(dir) = git_exe.parent() {
        let dir_name = dir
            .file_name()
            .map(|s| s.to_string_lossy().to_ascii_lowercase());
        match dir_name.as_deref() {
            // <root>\cmd\git.exe → <root>\bin\bash.exe
            Some("cmd") => {
                if let Some(root) = dir.parent() {
                    out.push(root.join("bin").join("bash.exe"));
                }
            }
            // <root>\mingw64\bin\git.exe → <root>\bin\bash.exe
            Some("bin") => {
                if let Some(mingw) = dir.parent()
                    && mingw.file_name().is_some_and(|n| {
                        n.to_string_lossy()
                            .to_ascii_lowercase()
                            .starts_with("mingw")
                    })
                    && let Some(root) = mingw.parent()
                {
                    out.push(root.join("bin").join("bash.exe"));
                }
                // <root>\bin\git.exe → sibling bash.exe
                out.push(dir.join("bash.exe"));
            }
            _ => out.push(dir.join("bash.exe")),
        }
    }
    out
}

/// Dropdown parity: locate Git Bash. Order: the Git-for-Windows
/// registry key (HKLM → HKCU → the WOW6432Node 32-bit view), the well-known
/// install dirs, then derive from a `git.exe` on `PATH`. All checks are
/// local filesystem/registry reads (microseconds) — no worker thread needed.
#[cfg(windows)]
fn git_bash_path() -> Option<std::path::PathBuf> {
    use winreg::RegKey;
    use winreg::enums::{HKEY_CURRENT_USER, HKEY_LOCAL_MACHINE};
    let exists = |p: &std::path::Path| {
        std::fs::symlink_metadata(p)
            .map(|m| !m.is_dir())
            .unwrap_or(false)
    };
    let from_install_dir = |root: String| {
        let bash = std::path::PathBuf::from(root).join("bin").join("bash.exe");
        exists(&bash).then_some(bash)
    };
    for (hive, key) in [
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\GitForWindows"),
        (HKEY_CURRENT_USER, r"SOFTWARE\GitForWindows"),
        (HKEY_LOCAL_MACHINE, r"SOFTWARE\WOW6432Node\GitForWindows"),
    ] {
        if let Ok(k) = RegKey::predef(hive).open_subkey(key)
            && let Ok(install) = k.get_value::<String, _>("InstallPath")
            && let Some(bash) = from_install_dir(install)
        {
            return Some(bash);
        }
    }
    for base in ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"] {
        if let Some(v) = std::env::var_os(base) {
            let root = if base == "LOCALAPPDATA" {
                std::path::PathBuf::from(v).join("Programs").join("Git")
            } else {
                std::path::PathBuf::from(v).join("Git")
            };
            let bash = root.join("bin").join("bash.exe");
            if exists(&bash) {
                return Some(bash);
            }
        }
    }
    let git = find_on_path("git.exe")?;
    git_bash_from_git_exe(&git).into_iter().find(|p| exists(p))
}

#[cfg(not(windows))]
fn unix_on_path(exe: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path| {
            std::env::split_paths(&path).any(|dir| {
                std::fs::metadata(dir.join(exe))
                    .map(|m| m.is_file())
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

#[cfg(not(windows))]
fn detect_shells_unix(
    shell_env: Option<String>,
    available: impl Fn(&str) -> bool,
) -> Vec<ShellChoice> {
    let basename = |p: &str| p.rsplit('/').next().unwrap_or(p).to_string();
    let mut out: Vec<ShellChoice> = Vec::new();
    if let Some(s) = shell_env.filter(|s| !s.is_empty()) {
        out.push((basename(&s), vec![s]));
    }
    for sh in ["bash", "zsh", "fish"] {
        if available(sh) && !out.iter().any(|(_, argv)| basename(&argv[0]) == sh) {
            out.push((sh.to_string(), vec![sh.to_string()]));
        }
    }
    if out.is_empty() {
        out.push(("Shell".to_string(), vec!["/bin/sh".to_string()]));
    }
    out
}

/// Parse distro names from `wsl.exe -l -q` output — one per line,
/// stripping a UTF-16 BOM artifact, surrounding whitespace, and trailing NULs,
/// dropping blanks. Pure → unit-testable without wsl.exe (built in test or on
/// Windows where `list_wsl_distros` calls it).
#[cfg(any(windows, test))]
fn parse_wsl_distros(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}' || c == '\u{0}'))
        .filter(|l| !l.is_empty())
        .map(|l| l.to_string())
        .collect()
}

/// Installed WSL distros via `wsl.exe -l -q` (bare names, no header).
/// Output is UTF-16-LE; decode then parse. Empty on any spawn/exit failure so a
/// host without WSL simply offers no WSL entries.
#[cfg(windows)]
fn list_wsl_distros() -> Vec<String> {
    // Run `wsl.exe -l -q` on a worker thread with a bounded
    // wait. The dropdown that calls this (new-tab `▾`) runs on the UI thread, so
    // a wedged LxssManager — the very `Wsl/Service/E_UNEXPECTED` state that
    // freezes `wsl.exe` — would otherwise hang the whole window ("not
    // responding"). On timeout we abandon the call and report no distros; the
    // worker self-terminates if `wsl.exe` ever returns (its `send` no-ops once
    // the receiver is gone). With the App-side cache (open_new_tab_menu), the
    // worst case is one ~2 s wait on the first dropdown open. `-l -q` only reads
    // the registry/service (it doesn't boot a distro), so 2 s is generous.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(
            std::process::Command::new("wsl.exe")
                .args(["-l", "-q"])
                .output(),
        );
    });
    let Ok(Ok(out)) = rx.recv_timeout(std::time::Duration::from_secs(2)) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let units: Vec<u16> = out
        .stdout
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .collect();
    parse_wsl_distros(&String::from_utf16_lossy(&units))
}

/// One axis of a `PtySize`, computed without overflow. `cell` is the
/// per-cell pixel extent (1 when computing the row/column count itself);
/// `count` is the grid dimension in cells. The product is evaluated in
/// `u32` and clamped into `u16`, the type `PtySize` requires. The old
/// `cell_w * cols as u16` did the whole multiply in `u16` — a panic in
/// debug and a silent wrap in release once the product passed 65535,
/// reachable with a HiDPI cell on a very wide grid — and `cols as u16`
/// truncated a pathological `usize` before the multiply.
fn clamp_pty_dim(cell: u16, count: usize) -> u16 {
    let count = count.min(u16::MAX as usize) as u32;
    (cell as u32 * count).min(u16::MAX as u32) as u16
}

// portable-pty's ConPTY backend converts rows/columns to a signed `COORD`.
// Clamp before crossing that boundary so a grid above 32767 cannot wrap
// negative. Unix winsize fields are unsigned 16-bit.
#[cfg(windows)]
const NATIVE_PTY_GRID_MAX: usize = i16::MAX as usize;
#[cfg(not(windows))]
const NATIVE_PTY_GRID_MAX: usize = u16::MAX as usize;

fn clamp_native_pty_grid(count: usize) -> u16 {
    count.min(NATIVE_PTY_GRID_MAX) as u16
}

fn native_pty_size(geometry: PtyGeometry) -> PtySize {
    PtySize {
        rows: clamp_native_pty_grid(geometry.rows),
        cols: clamp_native_pty_grid(geometry.columns),
        pixel_width: geometry.pixel_width,
        pixel_height: geometry.pixel_height,
    }
}

fn native_resize_required(applied: PtyGeometry, desired: PtyGeometry) -> bool {
    let applied_size = native_pty_size(applied);
    let desired_size = native_pty_size(desired);
    #[cfg(windows)]
    {
        // ConPTY ignores the pixel fields and ResizePseudoConsole is a
        // synchronous call. Avoid it for fractional-DPI pixel-only changes.
        applied_size.rows != desired_size.rows || applied_size.cols != desired_size.cols
    }
    #[cfg(not(windows))]
    {
        applied_size.rows != desired_size.rows
            || applied_size.cols != desired_size.cols
            || applied_size.pixel_width != desired_size.pixel_width
            || applied_size.pixel_height != desired_size.pixel_height
    }
}

/// The default shell `CommandBuilder` when no `command` is
/// configured. Windows prefers pwsh 7 → Windows PowerShell → `%ComSpec%`;
/// every other platform defers to portable_pty (which honors `$SHELL`).
fn default_prog() -> CommandBuilder {
    #[cfg(windows)]
    {
        if let Some(path) = pick_windows_default_shell(find_on_path) {
            return CommandBuilder::new(path);
        }
    }
    CommandBuilder::new_default_prog()
}

/// v2.29.1: the default-shell `CommandBuilder`, optionally auto-injecting
/// kettle's shell integration so the shell reports its working directory
/// (OSC 7) + prompt marks (OSC 133) with zero `$PROFILE` setup. This is what
/// lets the tab track `cd` for a stock PowerShell — whose `Set-Location` does
/// NOT update the OS process cwd, so it is unreadable from outside the process.
///
/// On Windows, pwsh/powershell are launched with a short ASCII bootstrap that
/// decodes the embedded UTF-8 integration. The user's `$PROFILE` still loads
/// first, then kettle's hook wraps the resulting prompt.
/// cmd.exe is left untouched — its process cwd already tracks `cd` (read by the
/// native poll). `inject = false` (config `shell-integration = off`) reproduces
/// the bare [`default_prog`]. Unix-shell rc-hook injection is a follow-up; on
/// non-Windows this currently defers to [`default_prog`].
fn default_prog_with_integration(inject: bool) -> CommandBuilder {
    #[cfg(windows)]
    if inject
        && let Some(path) = pick_windows_default_shell(find_on_path)
        && is_powershell(&path)
    {
        return powershell_integration_command(&path);
    }
    #[cfg(not(windows))]
    let _ = inject;
    default_prog()
}

/// v2.29.1: the kettle PowerShell shell-integration body, embedded so the
/// spawned pwsh can be launched already wired (no `$PROFILE` edit needed).
#[cfg(windows)]
const POWERSHELL_INTEGRATION: &str = include_str!("../../../shell-integration/kettle.ps1");

/// v2.29.1: is `path` a PowerShell (pwsh / powershell) executable, by basename?
#[cfg(windows)]
fn is_powershell(path: &std::path::Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .map(|s| {
            matches!(
                s.to_ascii_lowercase().as_str(),
                "pwsh.exe" | "powershell.exe" | "pwsh" | "powershell"
            )
        })
        .unwrap_or(false)
}

/// Windows limits a process command line to 32,767 UTF-16 code units.
/// `-EncodedCommand` base64-encodes UTF-16LE, so the integration crossed that
/// limit when completion support was added. Encoding the source as UTF-8 and
/// decoding it in a fixed ASCII bootstrap keeps the same quoting safety while
/// using roughly half the command line. The compile-time cap leaves room for
/// the executable path, quoting, and future arguments.
#[cfg(windows)]
fn powershell_integration_command(path: &std::path::Path) -> CommandBuilder {
    let mut c = CommandBuilder::new(path);
    c.arg("-NoExit");
    c.arg("-Command");
    c.arg(powershell_integration_bootstrap(POWERSHELL_INTEGRATION));
    c
}

#[cfg(windows)]
const POWERSHELL_BOOTSTRAP_PREFIX: &str =
    "& ([scriptblock]::Create([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('";
#[cfg(windows)]
const POWERSHELL_BOOTSTRAP_SUFFIX: &str = "'))))";
#[cfg(windows)]
const POWERSHELL_BOOTSTRAP_MAX_CHARS: usize = 24_000;

// Keep the embedded command comfortably below CreateProcessW's 32,767-code-unit
// ceiling. The payload is ASCII, so bytes and UTF-16 code units are identical.
#[cfg(windows)]
const _: () = assert!(
    POWERSHELL_BOOTSTRAP_PREFIX.len()
        + POWERSHELL_INTEGRATION.len().div_ceil(3) * 4
        + POWERSHELL_BOOTSTRAP_SUFFIX.len()
        <= POWERSHELL_BOOTSTRAP_MAX_CHARS
);

#[cfg(windows)]
fn powershell_integration_bootstrap(script: &str) -> String {
    let encoded = base64_standard(script.as_bytes());
    format!("{POWERSHELL_BOOTSTRAP_PREFIX}{encoded}{POWERSHELL_BOOTSTRAP_SUFFIX}")
}

/// Minimal standard-alphabet base64 encoder (padded). Self-contained so
/// kettle-core takes no base64 dependency for this one-shot spawn-time encode.
#[cfg(windows)]
fn base64_standard(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(A[((n >> 18) & 63) as usize] as char);
        out.push(A[((n >> 12) & 63) as usize] as char);
        out.push(if chunk.len() > 1 {
            A[((n >> 6) & 63) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            A[(n & 63) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// Cap on the OSC 133 prompt-mark ring. A long-lived shell
/// session emits one mark per prompt; without a cap the Vec grew unbounded.
const MAX_PROMPT_MARKS: usize = 2048;

/// Convert a grid-relative line to a monotonic document-row id.
fn stable_grid_line_id(history_origin: u64, history_size: usize, line: i32) -> u64 {
    let screen_top = history_origin.saturating_add(history_size as u64);
    if line < 0 {
        screen_top.saturating_sub(line.unsigned_abs() as u64)
    } else {
        screen_top.saturating_add(line as u64)
    }
}

/// Push a stable prompt-start row id into the bounded ring.
/// Dedups against the most-recent mark (some shells emit OSC 133 `A` twice for a
/// single prompt) and trims oldest-first with O(1) `pop_front` — the previous
/// `Vec::drain(0..d)` shifted all ~2048 elements on every prompt once full, on
/// the hot reader-thread path. Pure, so the ring discipline is unit-tested.
fn push_prompt_mark(ring: &mut std::collections::VecDeque<u64>, row_id: u64) {
    if ring.back() == Some(&row_id) {
        return;
    }
    ring.push_back(row_id);
    while ring.len() > MAX_PROMPT_MARKS {
        ring.pop_front();
    }
}

fn prompt_navigation_offset(
    ring: &mut std::collections::VecDeque<u64>,
    history_origin: u64,
    history_size: usize,
    screen_lines: usize,
    display_offset: usize,
    previous: bool,
) -> Option<usize> {
    let screen_top = history_origin.saturating_add(history_size as u64);
    let retained_end = screen_top.saturating_add(screen_lines as u64);
    ring.retain(|mark| *mark >= history_origin && *mark < retained_end);

    let current_top = screen_top.saturating_sub(display_offset.min(history_size) as u64);
    let target = if previous {
        ring.iter()
            .copied()
            .filter(|mark| *mark < current_top)
            .max()
    } else {
        ring.iter()
            .copied()
            .filter(|mark| *mark > current_top)
            .min()
    }?;

    let offset = screen_top.saturating_sub(target).min(history_size as u64);
    Some(usize::try_from(offset).unwrap_or(history_size))
}

#[derive(Clone, Copy)]
struct KittyDeleteGeometry {
    screen_top: u64,
    screen_lines: usize,
    cursor_abs_line: u64,
    cursor_col: usize,
}

fn row_span_contains(start: u64, rows: usize, row: u64) -> bool {
    rows != 0
        && u128::from(start) <= u128::from(row)
        && u128::from(row) < u128::from(start) + rows as u128
}

fn row_spans_intersect(
    first_start: u64,
    first_rows: usize,
    second_start: u64,
    second_rows: usize,
) -> bool {
    if first_rows == 0 || second_rows == 0 {
        return false;
    }
    let first_start = u128::from(first_start);
    let second_start = u128::from(second_start);
    let first_end = first_start + first_rows as u128;
    let second_end = second_start + second_rows as u128;
    first_start < second_end && second_start < first_end
}

fn placement_intersects_cell(placement: &Placement, abs_line: u64, col: usize) -> bool {
    placement.cell_cols != 0
        && row_span_contains(placement.abs_line, placement.cell_rows, abs_line)
        && placement.col <= col
        && col < placement.col.saturating_add(placement.cell_cols)
}

fn kitty_delete_matches_placement(
    delete: &KittyDelete,
    placement: &Placement,
    geometry: KittyDeleteGeometry,
) -> bool {
    let Some(id) = placement.id else {
        // A kitty command must never delete Sixel or iTerm2 placements.
        return false;
    };
    match delete.target {
        KittyDeleteTarget::Visible => row_spans_intersect(
            placement.abs_line,
            placement.cell_rows,
            geometry.screen_top,
            geometry.screen_lines,
        ),
        KittyDeleteTarget::Image {
            id: wanted,
            placement_id,
        } => id == wanted && placement_id.is_none_or(|wanted| wanted == placement.placement_id),
        KittyDeleteTarget::Cursor => {
            placement_intersects_cell(placement, geometry.cursor_abs_line, geometry.cursor_col)
        }
        KittyDeleteTarget::Cell { x, y } => {
            x.checked_sub(1)
                .zip(y.checked_sub(1))
                .is_some_and(|(col, row)| {
                    placement_intersects_cell(
                        placement,
                        geometry.screen_top.saturating_add(row as u64),
                        col as usize,
                    )
                })
        }
        KittyDeleteTarget::CellAtZ { x, y, z } => {
            placement.z == z
                && x.checked_sub(1)
                    .zip(y.checked_sub(1))
                    .is_some_and(|(col, row)| {
                        placement_intersects_cell(
                            placement,
                            geometry.screen_top.saturating_add(row as u64),
                            col as usize,
                        )
                    })
        }
        KittyDeleteTarget::IdRange { first, last } => first <= id && id <= last,
        KittyDeleteTarget::Column { x } => x.checked_sub(1).is_some_and(|col| {
            placement.cell_cols != 0
                && placement.col <= col as usize
                && (col as usize) < placement.col.saturating_add(placement.cell_cols)
        }),
        KittyDeleteTarget::Row { y } => y.checked_sub(1).is_some_and(|row| {
            let abs_line = geometry.screen_top.saturating_add(row as u64);
            row_span_contains(placement.abs_line, placement.cell_rows, abs_line)
        }),
        KittyDeleteTarget::ZIndex { z } => placement.z == z,
    }
}

fn kitty_delete_matches_virtual(delete: &KittyDelete, image_id: u32, placement_id: u32) -> bool {
    match delete.target {
        KittyDeleteTarget::Image {
            id,
            placement_id: wanted,
        } => id == image_id && wanted.is_none_or(|wanted| wanted == placement_id),
        KittyDeleteTarget::IdRange { first, last } => first <= image_id && image_id <= last,
        // The kitty spec explicitly excludes virtual placements from every
        // spatial selector, including visible-all and z-index deletion.
        _ => false,
    }
}

fn kitty_delete_freed_ids(
    delete: &KittyDelete,
    candidates: &std::collections::HashSet<u32>,
    referenced: &std::collections::HashSet<u32>,
) -> Vec<u32> {
    if !delete.free_data {
        return Vec::new();
    }
    let mut freed = candidates
        .iter()
        .copied()
        .filter(|id| !referenced.contains(id))
        .collect::<Vec<_>>();
    freed.sort_unstable();
    freed
}

#[cfg(test)]
mod kitty_delete_tests {
    use std::collections::HashSet;

    use kettle_vt::kitty::{Delete, DeleteTarget};

    use super::{
        KittyDeleteGeometry, Placement, kitty_delete_freed_ids, kitty_delete_matches_placement,
        kitty_delete_matches_virtual,
    };
    use crate::ImageData;

    fn delete(target: DeleteTarget, free_data: bool) -> Delete {
        Delete {
            target,
            free_data,
            free_candidates: Vec::new(),
        }
    }

    fn placement() -> Placement {
        Placement {
            abs_line: 101,
            col: 3,
            cell_cols: 4,
            cell_rows: 3,
            x_offset_cells: 0.0,
            y_offset_cells: 0.0,
            display_cols: 4.0,
            display_rows: 3.0,
            img: ImageData::new(1, 1, vec![1, 2, 3, 255]).expect("pixel"),
            source_rect: None,
            source_crop: None,
            id: Some(10),
            placement_id: 7,
            kitty_params: None,
            z: -2,
        }
    }

    fn geometry() -> KittyDeleteGeometry {
        KittyDeleteGeometry {
            screen_top: 100,
            screen_lines: 4,
            cursor_abs_line: 102,
            cursor_col: 4,
        }
    }

    #[test]
    fn all_kitty_delete_selectors_match_exact_placement_geometry() {
        let placement = placement();
        let geometry = geometry();
        let matching = [
            DeleteTarget::Visible,
            DeleteTarget::Image {
                id: 10,
                placement_id: None,
            },
            DeleteTarget::Image {
                id: 10,
                placement_id: Some(7),
            },
            DeleteTarget::Cursor,
            DeleteTarget::Cell { x: 4, y: 2 },
            DeleteTarget::CellAtZ { x: 4, y: 2, z: -2 },
            DeleteTarget::IdRange { first: 9, last: 11 },
            DeleteTarget::Column { x: 7 },
            DeleteTarget::Row { y: 3 },
            DeleteTarget::ZIndex { z: -2 },
        ];
        for target in matching {
            assert!(
                kitty_delete_matches_placement(
                    &delete(target.clone(), false),
                    &placement,
                    geometry
                ),
                "{target:?} should match"
            );
        }

        let nonmatching = [
            DeleteTarget::Image {
                id: 11,
                placement_id: None,
            },
            DeleteTarget::Image {
                id: 10,
                placement_id: Some(8),
            },
            DeleteTarget::Cell { x: 2, y: 2 },
            DeleteTarget::Cell { x: 4, y: 0 },
            DeleteTarget::CellAtZ { x: 4, y: 2, z: 0 },
            DeleteTarget::IdRange {
                first: 11,
                last: 20,
            },
            DeleteTarget::Column { x: 8 },
            DeleteTarget::Row { y: 5 },
            DeleteTarget::ZIndex { z: 2 },
        ];
        for target in nonmatching {
            assert!(
                !kitty_delete_matches_placement(
                    &delete(target.clone(), false),
                    &placement,
                    geometry
                ),
                "{target:?} should not match"
            );
        }
    }

    #[test]
    fn visible_delete_keeps_scrollback_and_kitty_never_deletes_other_protocols() {
        let mut above = placement();
        above.abs_line = 96;
        above.cell_rows = 4; // bottom is exactly the active-screen origin.
        assert!(!kitty_delete_matches_placement(
            &delete(DeleteTarget::Visible, false),
            &above,
            geometry()
        ));

        let mut below = placement();
        below.abs_line = 104; // starts exactly below the active screen.
        assert!(!kitty_delete_matches_placement(
            &delete(DeleteTarget::Visible, false),
            &below,
            geometry()
        ));
        below.abs_line = 103; // one visible row, remainder below the screen.
        assert!(kitty_delete_matches_placement(
            &delete(DeleteTarget::Visible, false),
            &below,
            geometry()
        ));

        let mut sixel = placement();
        sixel.id = None;
        assert!(!kitty_delete_matches_placement(
            &delete(DeleteTarget::ZIndex { z: -2 }, false),
            &sixel,
            geometry()
        ));
    }

    #[test]
    fn virtual_placements_only_match_id_and_range_selectors() {
        assert!(kitty_delete_matches_virtual(
            &delete(
                DeleteTarget::Image {
                    id: 10,
                    placement_id: Some(7)
                },
                false
            ),
            10,
            7
        ));
        assert!(kitty_delete_matches_virtual(
            &delete(DeleteTarget::IdRange { first: 9, last: 11 }, false),
            10,
            7
        ));
        for target in [
            DeleteTarget::Visible,
            DeleteTarget::Cursor,
            DeleteTarget::Cell { x: 1, y: 1 },
            DeleteTarget::CellAtZ { x: 1, y: 1, z: 0 },
            DeleteTarget::Column { x: 1 },
            DeleteTarget::Row { y: 1 },
            DeleteTarget::ZIndex { z: 0 },
        ] {
            assert!(
                !kitty_delete_matches_virtual(&delete(target, false), 10, 7),
                "spatial selectors must not delete virtual prototypes"
            );
        }
    }

    #[test]
    fn uppercase_frees_only_unreferenced_candidates() {
        let candidates = HashSet::from([1, 2]);
        let referenced = HashSet::from([2]);
        assert!(
            kitty_delete_freed_ids(
                &delete(DeleteTarget::Visible, false),
                &candidates,
                &referenced
            )
            .is_empty()
        );
        assert_eq!(
            kitty_delete_freed_ids(
                &delete(DeleteTarget::Visible, true),
                &candidates,
                &referenced
            ),
            vec![1]
        );
    }
}

/// Estimated retained bytes for one scrollback line.
///
/// This counts the inline grid representation only. A cell can also own heap
/// storage — combining marks, an underline color, a hyperlink — which this
/// deliberately does not walk: doing so would mean touching every cell of every
/// line on each budget evaluation, on the PTY reader's path.
///
/// That is sound only because the dynamic part is separately bounded. Combining
/// marks are capped per cell (`MAX_ZEROWIDTH_PER_CELL`); before that cap a
/// single cell could grow with the entire input while this estimate stayed
/// flat, which made the configured `scrollback-bytes` ceiling meaningless.
/// Hyperlink storage is shared behind an `Arc` across the cells of one link.
/// So the estimate understates by a bounded factor rather than an unbounded
/// one — see `docs/CONFIG.md`, which says the same thing to users.
/// The errno meaning "no such process", used to recognize a child that exited
/// before the kill reached it.
///
/// Windows has no ESRCH; its equivalent race surfaces as access-denied on a
/// dead handle and is normalized inside the vendored PTY layer, so a value
/// that never matches a real Windows error is correct there.
const fn libc_esrch() -> i32 {
    #[cfg(unix)]
    {
        libc::ESRCH
    }
    #[cfg(not(unix))]
    {
        i32::MIN
    }
}

fn scrollback_line_bytes(columns: usize) -> usize {
    const ROW_OVERHEAD_BYTES: usize = 64;
    let columns = columns.max(1);
    std::mem::size_of::<Cell>()
        .saturating_mul(columns)
        .saturating_add(ROW_OVERHEAD_BYTES)
        .max(1)
}

fn effective_scrollback_lines(
    configured_lines: usize,
    configured_bytes: usize,
    columns: usize,
    screen_lines: usize,
) -> usize {
    if configured_bytes == 0 {
        return configured_lines;
    }
    let line_bytes = scrollback_line_bytes(columns);
    let visible_bytes = line_bytes.saturating_mul(screen_lines.max(1));
    let byte_lines = configured_bytes.saturating_sub(visible_bytes) / line_bytes;
    configured_lines.min(byte_lines)
}

/// The scrollback cap a pane should carry after a geometry change.
///
/// Monotonic for the life of the pane: it can rise, never fall.
///
/// [`effective_scrollback_lines`] turns the byte budget into a line count by
/// dividing it by a worst-case per-row cost at the CURRENT column count, so a
/// wider pane yields a smaller number. Feeding that straight back to the grid
/// meant every widen handed `Grid::update_history` a lower limit, and it trims
/// from the oldest end — immediately, and with no way to get those rows back.
/// Four ordinary gestures reached it: dragging a window wider (each
/// intermediate width applying its own cap), decrease-font, closing a sibling
/// split, and un-zooming.
///
/// Nothing about a resize means the user wants less history, so a resize must
/// not be how the budget is enforced. Every other terminal bounds scrollback in
/// LINES and none of them evicts on widening; the one with a real byte budget
/// (Ghostty) trims from the oldest end as new output arrives, which is a
/// property of growth rather than of geometry.
fn scrollback_cap_after_resize(
    current: usize,
    configured_lines: usize,
    configured_bytes: usize,
    columns: usize,
    screen_lines: usize,
) -> usize {
    current.max(effective_scrollback_lines(
        configured_lines,
        configured_bytes,
        columns,
        screen_lines,
    ))
}

fn reported_current_dir(osc_cwd_seen: bool, cwd: Option<String>) -> Option<String> {
    osc_cwd_seen.then_some(cwd).flatten()
}

impl Terminal {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        argv: &[String],
        cwd: Option<&str>,
        scrollback: usize,
        cols: usize,
        rows: usize,
        cell_w: u16,
        cell_h: u16,
        cursor_blink: bool,
        cursor_shape: CursorShape,
        word_delimiters: Option<&str>,
        event_tx: crossbeam_channel::Sender<TermEvent>,
        waker: Waker,
    ) -> Result<Terminal> {
        Self::new_with_env(
            argv,
            cwd,
            scrollback,
            0,
            cols,
            rows,
            cell_w,
            cell_h,
            cursor_blink,
            cursor_shape,
            word_delimiters,
            "xterm-256color",
            "truecolor",
            false,
            event_tx,
            waker,
        )
    }

    /// Terminator parity: PTY spawn with explicit `TERM` +
    /// `COLORTERM` env override + `login_shell` flag (prepends `-l`
    /// to the shell argv to get login-shell semantics).
    ///
    /// `term` / `colorterm` correspond to Terminator's per-profile
    /// `term` (terminatorlib/config.py:114) and `colorterm`
    /// (`:115`); `login_shell` is `:122`. Empty strings preserve
    /// kettle's existing default — same shape as the parse-side
    /// fall-through.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_env(
        argv: &[String],
        cwd: Option<&str>,
        scrollback: usize,
        scrollback_bytes: usize,
        cols: usize,
        rows: usize,
        cell_w: u16,
        cell_h: u16,
        cursor_blink: bool,
        cursor_shape: CursorShape,
        word_delimiters: Option<&str>,
        term_env: &str,
        colorterm_env: &str,
        login_shell: bool,
        event_tx: crossbeam_channel::Sender<TermEvent>,
        waker: Waker,
    ) -> Result<Terminal> {
        Self::new_with_env_and_output(
            argv,
            cwd,
            scrollback,
            scrollback_bytes,
            cols,
            rows,
            cell_w,
            cell_h,
            cursor_blink,
            cursor_shape,
            word_delimiters,
            term_env,
            colorterm_env,
            &[],
            login_shell,
            // Legacy shim (no in-tree live callers; tests pass explicit argv) —
            // never auto-inject; the Mux spawn path passes the real config.
            false,
            event_tx,
            waker,
            None,
        )
    }

    /// Terminator plugin parity: same
    /// as `new_with_env` plus an optional sidechannel that ships raw
    /// PTY-output bytes to the App for `LuaEvent::Output` dispatch.
    /// `None` keeps the zero-cost path for non-Lua kettle runs;
    /// `Some(tx)` lets a plugin-runtime caller subscribe.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_env_and_output(
        argv: &[String],
        cwd: Option<&str>,
        scrollback: usize,
        scrollback_bytes: usize,
        cols: usize,
        rows: usize,
        cell_w: u16,
        cell_h: u16,
        cursor_blink: bool,
        cursor_shape: CursorShape,
        word_delimiters: Option<&str>,
        term_env: &str,
        colorterm_env: &str,
        extra_env: &[(String, String)],
        login_shell: bool,
        shell_integration: bool,
        event_tx: crossbeam_channel::Sender<TermEvent>,
        waker: Waker,
        output_tx: Option<PtyOutputSender>,
    ) -> Result<Terminal> {
        Self::new_with_env_and_output_geometry(
            argv,
            cwd,
            scrollback,
            scrollback_bytes,
            PtyGeometry::from_cell_size(cols, rows, cell_w, cell_h),
            cursor_blink,
            cursor_shape,
            word_delimiters,
            term_env,
            colorterm_env,
            extra_env,
            login_shell,
            shell_integration,
            event_tx,
            waker,
            output_tx,
        )
    }

    /// Spawn with exact initial grid and text-area pixel geometry.
    ///
    /// Live UI callers use this entry point so the first ConPTY/openpty size
    /// already reflects fractional DPI. The compatibility constructor above
    /// derives totals from integer cell metrics for headless/legacy callers.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_env_and_output_geometry(
        argv: &[String],
        cwd: Option<&str>,
        scrollback: usize,
        scrollback_bytes: usize,
        geometry: PtyGeometry,
        cursor_blink: bool,
        cursor_shape: CursorShape,
        word_delimiters: Option<&str>,
        term_env: &str,
        colorterm_env: &str,
        extra_env: &[(String, String)],
        login_shell: bool,
        shell_integration: bool,
        event_tx: crossbeam_channel::Sender<TermEvent>,
        waker: Waker,
        output_tx: Option<PtyOutputSender>,
    ) -> Result<Terminal> {
        Self::new_with_env_and_output_geometry_and_capabilities(
            argv,
            cwd,
            scrollback,
            scrollback_bytes,
            geometry,
            cursor_blink,
            cursor_shape,
            word_delimiters,
            term_env,
            colorterm_env,
            extra_env,
            login_shell,
            shell_integration,
            TerminalCapabilities::default(),
            event_tx,
            waker,
            output_tx,
        )
    }

    /// Spawn with exact geometry and explicit runtime protocol capabilities.
    ///
    /// The capability object lets a UI report policy-dependent features
    /// truthfully without changing the compatibility constructors above.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_env_and_output_geometry_and_capabilities(
        argv: &[String],
        cwd: Option<&str>,
        scrollback: usize,
        scrollback_bytes: usize,
        geometry: PtyGeometry,
        cursor_blink: bool,
        cursor_shape: CursorShape,
        word_delimiters: Option<&str>,
        term_env: &str,
        colorterm_env: &str,
        extra_env: &[(String, String)],
        login_shell: bool,
        shell_integration: bool,
        capabilities: TerminalCapabilities,
        event_tx: crossbeam_channel::Sender<TermEvent>,
        waker: Waker,
        output_tx: Option<PtyOutputSender>,
    ) -> Result<Terminal> {
        Self::new_with_env_and_output_geometry_capabilities_and_cwd_policy(
            argv,
            cwd,
            scrollback,
            scrollback_bytes,
            geometry,
            cursor_blink,
            cursor_shape,
            word_delimiters,
            term_env,
            colorterm_env,
            extra_env,
            login_shell,
            shell_integration,
            capabilities,
            WorkingDirectoryPolicy::FallbackToHome,
            event_tx,
            waker,
            output_tx,
        )
    }

    /// Spawn with exact geometry, capabilities, and explicit cwd semantics.
    ///
    /// Headless automation uses [`WorkingDirectoryPolicy::RejectInvalidExplicit`]
    /// so the OS rejects a path that disappears after validation. Interactive
    /// panes retain the recovery behavior of the compatibility constructors.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_env_and_output_geometry_capabilities_and_cwd_policy(
        argv: &[String],
        cwd: Option<&str>,
        scrollback: usize,
        scrollback_bytes: usize,
        geometry: PtyGeometry,
        cursor_blink: bool,
        cursor_shape: CursorShape,
        word_delimiters: Option<&str>,
        term_env: &str,
        colorterm_env: &str,
        extra_env: &[(String, String)],
        login_shell: bool,
        shell_integration: bool,
        capabilities: TerminalCapabilities,
        cwd_policy: WorkingDirectoryPolicy,
        event_tx: crossbeam_channel::Sender<TermEvent>,
        waker: Waker,
        output_tx: Option<PtyOutputSender>,
    ) -> Result<Terminal> {
        let cols = geometry.columns;
        let rows = geometry.rows;
        let pty = portable_pty::native_pty_system();
        // ConPTY's signed COORD and Unix's unsigned winsize have different grid
        // bounds; `native_pty_size` applies the platform contract before the
        // backend sees it. Exact total pixels are preserved on Unix.
        let pair = pty.openpty(native_pty_size(geometry))?;

        let mut cmd = match argv.split_first() {
            Some((prog, rest)) => {
                let mut c = CommandBuilder::new(prog);
                if login_shell && prog_accepts_login_flag(prog) {
                    // `-l` (POSIX-defined "shell that
                    // reads /etc/profile + ~/.profile + login dotfiles
                    // before running interactively"). Goes BEFORE
                    // the user's argv args so a config like
                    // `command = bash -i` still works.
                    // Skipped for `wsl.exe` (where `-l` lists
                    // distros) and Windows-native shells (pwsh/powershell/cmd
                    // reject it) via `prog_accepts_login_flag`.
                    c.arg("-l");
                }
                for a in rest {
                    c.arg(a);
                }
                c
            }
            None => {
                let mut c = default_prog_with_integration(shell_integration);
                // `-l` is the POSIX login-shell switch. On
                // Windows `default_prog()` resolves to pwsh/powershell/cmd, none
                // of which accept it (powershell.exe errors on an unknown arg,
                // pwsh's `-Login` is reserved/no-op on Windows, cmd ignores it),
                // so `login-shell = true` with no explicit `command` produced a
                // broken/empty pane. The explicit-argv arm already guards the
                // analogous `wsl.exe` footgun; guard the default-shell arm for
                // Windows-native shells via `default_shell_accepts_login_flag`.
                if login_shell && default_shell_accepts_login_flag() {
                    c.arg("-l");
                }
                c
            }
        };
        #[cfg(windows)]
        overlay_windows_parent_env(&mut cmd, std::env::vars_os());
        cmd.set_process_tree_containment(capabilities.contain_process_tree);
        // Apply user pane env before terminal identity env so `term` /
        // `colorterm` stay authoritative for those protocol-critical values.
        for (name, value) in extra_env {
            cmd.env(name, value);
        }
        // Honor cfg.term + cfg.colorterm (empty preserves
        // kettle's default).
        cmd.env(
            "TERM",
            if term_env.is_empty() {
                "xterm-256color"
            } else {
                term_env
            },
        );
        cmd.env(
            "COLORTERM",
            if colorterm_env.is_empty() {
                "truecolor"
            } else {
                colorterm_env
            },
        );
        cmd.env("TERM_PROGRAM", "kettle");
        // `TERM_PROGRAM_VERSION` is the de-facto pair to `TERM_PROGRAM`
        // (iTerm2 / kitty / WezTerm / Ghostty all set it). Neovim's
        // `:checkhealth provider`, fish's prompt themers, and various
        // diagnostic tools key off the pair when probing whether they're
        // running under a known modern terminal. Kettle's own crate
        // version is the obvious answer — populated from Cargo at build
        // time so a bumped `kettle/Cargo.toml` flows through with no
        // separate version string to keep in sync.
        cmd.env("TERM_PROGRAM_VERSION", env!("CARGO_PKG_VERSION"));
        // Env vars set on the child's *Windows* process do
        // NOT cross into a WSL distro unless listed in `WSLENV`. Without this,
        // `COLORTERM` is silently dropped at the WSL boundary, so a program
        // inside WSL (Ubuntu) that decides truecolor support from `$COLORTERM`
        // — rather than force-enabling it — falls back to 256-color and
        // renders washed-out, mis-mapped colors. Append our terminal-identity
        // vars to WSLENV (preserving any the user already set) with the `/u`
        // flag, i.e. "pass Windows→WSL only". `cmd.env` set them on the
        // Windows side just above, so WSLENV can reference them. Harmless when
        // the child isn't `wsl.exe` — it's just an extra, ignored env var.
        cmd.env(
            "WSLENV",
            child_wslenv(&std::env::var("WSLENV").unwrap_or_default(), extra_env),
        );
        match cwd {
            Some(d) if cwd_policy == WorkingDirectoryPolicy::RejectInvalidExplicit => cmd.cwd(d),
            None if cwd_policy == WorkingDirectoryPolicy::RejectInvalidExplicit => {}
            Some(d) if std::path::Path::new(d).is_dir() => cmd.cwd(d),
            _ => {
                // Recorded cwd is missing or no longer on disk (e.g.,
                // user moved the repo between sessions, or the `-d` arg
                // pointed at a since-deleted path). Fall back to the OS
                // home directory. The previous version only checked
                // `HOME`, which is unset on Windows by default — so
                // Windows users with a stale recorded cwd silently
                // ended up in whatever directory they happened to
                // launch kettle from. `home_dir_fallback` probes
                // `HOME` then `USERPROFILE` then `APPDATA`, in that
                // order, so all three platforms (Linux/macOS/Windows)
                // converge on the same "user-home" intent. Same shape
                // as an earlier macOS universal2 packaging fix — Linux+macOS
                // worked, Windows didn't, the env var probe order is
                // the difference.
                // Also gate the fallback on `is_dir`. The env
                // var could be set to something that exists but isn't a
                // directory (an exotic `HOME=/etc/passwd` misconfig, or
                // a path that's a regular file / symlink to a file) —
                // `cmd.cwd` would then hand the OS spawn an invalid
                // target. Treating that the same as "no home" lets
                // `portable_pty` inherit kettle's launch directory
                // (the same recovery as when no env
                // var was set or it was empty).
                if let Some(home) = home_dir_fallback(|k| std::env::var_os(k))
                    && home.is_dir()
                {
                    cmd.cwd(home);
                }
            }
        }
        #[cfg(unix)]
        let reader_poll_fd = pair
            .master
            .as_raw_fd()
            .context("PTY master has no Unix descriptor for reader polling")?;
        // Kettle serializes all input/replies through bounded priority
        // workers. On ConPTY this opts our side of the synchronous byte pipe
        // into PIPE_NOWAIT, so a child that stops reading cannot monopolize a
        // worker; other backends retain their native writer here.
        let writer = pair.master.take_nonblocking_writer()?;
        let mut reader = pair.master.try_clone_reader()?;

        let output_wake = Arc::new(OutputWakeGate::new(waker.clone()));
        let osc52_copy_allowed = Arc::new(AtomicBool::new(capabilities.osc52_copy));
        let event_overflowed = Arc::new(AtomicBool::new(false));
        let proxy = EventProxy::with_output_wake_osc52_and_overflow(
            event_tx,
            waker.clone(),
            output_wake.clone(),
            osc52_copy_allowed.clone(),
            event_overflowed.clone(),
        );
        let lifecycle_pending = proxy.lifecycle_pending();
        // Seed the engine's *default* cursor style from the user config; the
        // engine seeds `cursor_style` lazily from this, and programs can flip
        // both fields at runtime — `?12 h/l` for blinking (honored live via
        // `cursor_blinking()` below) and DECSCUSR `CSI Ps SP q` for shape
        // (honored live via the engine's `renderable_content().cursor.shape`,
        // read by the renderer per-frame).
        let default_cursor_style = alacritty_terminal::vte::ansi::CursorStyle {
            blinking: cursor_blink,
            shape: cursor_shape,
        };
        let history_limit = effective_scrollback_lines(scrollback, scrollback_bytes, cols, rows);
        let mut tconf = TermConfig {
            scrolling_history: history_limit,
            // Kettle's winit input path implements Kitty's negotiated CSI-u
            // protocol (including repeat/release, keypad identities, alternate
            // keys, and associated text), so the engine may answer `CSI ? u`
            // and honor the per-screen keyboard-mode stack.
            kitty_keyboard: true,
            unnegotiated_modified_enter: capabilities.unnegotiated_modified_enter,
            default_cursor_style,
            ..TermConfig::default()
        };
        // Word delimiters drive double-click word selection (and the
        // jump-to-prompt search). An empty config means "use the engine
        // default" — `",│`|:\"' ()[]{}<>\t"` — so users that don't set
        // anything still get sensible word boundaries.
        if let Some(wd) = word_delimiters
            && !wd.is_empty()
        {
            tconf.semantic_escape_chars = wd.to_string();
        }
        let term = Term::new(
            tconf.clone(),
            &TermSize {
                columns: cols,
                screen_lines: rows,
            },
            proxy.clone(),
        );
        let term: SharedTerm = Arc::new(Mutex::new(term));

        let images: Images = Arc::new(Mutex::new(Vec::new()));
        let virtuals: Virtuals = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let anims: Animations = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let relatives: Relatives = Arc::new(Mutex::new(std::collections::HashMap::new()));
        let inactive_graphics: InactiveGraphics =
            Arc::new(Mutex::new(BufferGraphicsState::default()));
        let graphics_gate = Arc::new(Mutex::new(()));
        let graphics_reflow_generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let prompts: Arc<Mutex<std::collections::VecDeque<u64>>> =
            Arc::new(Mutex::new(std::collections::VecDeque::new()));
        // Terminator parity (`command_notify.py`):
        // per-pane OSC 133 OutputStart timestamp + completed-command
        // event queue. Reader thread writes; App polls each tick.
        let output_started_at: Arc<Mutex<Option<std::time::Instant>>> = Arc::new(Mutex::new(None));
        let output_start_seen: Arc<std::sync::atomic::AtomicBool> =
            Arc::new(std::sync::atomic::AtomicBool::new(false));
        let output_start_seen_for_struct = output_start_seen.clone();
        let command_finished: Arc<Mutex<Vec<CommandFinished>>> = Arc::new(Mutex::new(Vec::new()));
        let protocol_notifications: Arc<Mutex<Vec<ProtocolNotification>>> =
            Arc::new(Mutex::new(Vec::new()));
        let completion_cell = Arc::new(Mutex::new(CompletionSlot::default()));
        let cwd_cell: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(cwd.map(|s| s.to_string())));
        // v2.29.0: OS-derived cwd fallback (populated by the App's process poll
        // for native shells with no OSC 7/9;9). Starts empty.
        let native_cwd_cell: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        // v2.29.1: set true once the shell actually REPORTS a cwd via OSC 7/9;9.
        // `cwd_cell` is pre-seeded with the launch directory (above), so without
        // this flag `current_dir_or_native` would always prefer that frozen seed
        // and never fall through to the live native poll — leaving the tab stuck
        // at the launch dir for a stock shell that emits no OSC 7. Only a real
        // report flips this and makes the OSC cwd authoritative.
        let osc_cwd_seen: Arc<std::sync::atomic::AtomicBool> =
            Arc::new(std::sync::atomic::AtomicBool::new(false));
        let osc_cwd_seen_for_struct = osc_cwd_seen.clone();
        // Latest OSC 9;4 taskbar-progress state from this pane.
        // The reader thread writes it; the App polls the focused pane's value
        // each frame and drives the OS taskbar indicator (pwsh 7 parity).
        let progress_cell: Arc<Mutex<Option<Progress>>> = Arc::new(Mutex::new(None));
        let shared_geometry = Arc::new(Mutex::new(VersionedPtyGeometry {
            geometry,
            generation: 0,
        }));
        // Terminator parity (`plugins/logger.py`): per-pane session log.
        // Default None; `Action::ToggleSessionLog` installs a bounded writer at
        // runtime. The reader thread retains only its nonblocking admission end.
        let session_log: Arc<Mutex<Option<AsyncFileWriter>>> = Arc::new(Mutex::new(None));
        let session_log_for_struct = session_log.clone();
        let log_active: Arc<std::sync::atomic::AtomicBool> =
            Arc::new(std::sync::atomic::AtomicBool::new(false));
        let log_active_for_struct = log_active.clone();
        let log_generation: Arc<std::sync::atomic::AtomicU64> =
            Arc::new(std::sync::atomic::AtomicU64::new(0));
        let log_generation_for_struct = log_generation.clone();
        // Terminator parity: when true, strip ANSI
        // escape sequences from the bytes before writing to the
        // log file. Default false preserves the raw-stream
        // behavior.
        let log_strip_ansi: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
        let log_strip_ansi_for_struct = log_strip_ansi.clone();
        // Teardown stop flag (see the `stop` struct field).
        let stop = Arc::new(AtomicBool::new(false));
        // Teardown drain flag (see the `drain_output` struct field).
        let drain_output = Arc::new(AtomicBool::new(false));
        // C4 (multi-window): per-pane output-generation counter (see the
        // `out_gen` struct field).
        let out_gen = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let out_gen_reader = out_gen.clone();
        let pty_read_progress = Arc::new(PtyReadProgressState::new());
        let direct_child_exit_at = Arc::new(Mutex::new(None));
        // A short-lived child can write and close the slave before a newly
        // spawned reader thread gets its first timeslice. Most PTY backends
        // retain that tail, but macOS discards it if no read is pending. The
        // readiness channel alone cannot prove that a thread was not
        // descheduled between its send and its first read, so Unix also hands
        // Kettle's slave descriptor to the pump after spawning. The pump keeps
        // that descriptor alive until it reads the first bytes, or observes a
        // silent direct child exit, making the tail durable across that gap.
        let (reader_ready_tx, reader_ready_rx) =
            crossbeam_channel::bounded::<Result<(), String>>(1);
        #[cfg(unix)]
        let (startup_slave_tx, startup_slave_rx) = crossbeam_channel::bounded::<(
            Box<dyn portable_pty::SlavePty + Send>,
            UnixPtyWatcher,
        )>(1);

        let reader_thread = {
            let term = term.clone();
            let images = images.clone();
            let virtuals = virtuals.clone();
            let anims = anims.clone();
            let relatives = relatives.clone();
            let inactive_graphics = inactive_graphics.clone();
            let graphics_gate = graphics_gate.clone();
            let graphics_reflow_generation = graphics_reflow_generation.clone();
            let prompts = prompts.clone();
            let output_started_at = output_started_at.clone();
            let output_start_seen = output_start_seen.clone();
            let command_finished = command_finished.clone();
            let protocol_notifications = protocol_notifications.clone();
            let completion_cell = completion_cell.clone();
            let cwd_cell = cwd_cell.clone();
            let osc_cwd_seen = osc_cwd_seen.clone();
            let progress_cell = progress_cell.clone();
            let shared_geometry = shared_geometry.clone();
            let output_wake = output_wake.clone();
            let session_log = session_log.clone();
            let log_strip_ansi = log_strip_ansi.clone();
            let log_active = log_active.clone();
            let log_generation = log_generation.clone();
            let stop = stop.clone();
            let drain_output = drain_output.clone();
            let reader_progress = Arc::clone(&pty_read_progress);
            #[cfg(unix)]
            let reader_child_exit_at = Arc::clone(&direct_child_exit_at);
            std::thread::Builder::new()
                .name("kettle-pty-reader".into())
                .spawn(move || {
                    let mut processor: Processor = Processor::new();
                    let mut extractor = Extractor::new();
                    let mut active_alternate = false;
                    let mut observed_reflow_generation = 0;
                    let mut session_log_filter = SessionLogFilter::default();
                    // A blocking PTY read must remain on a pump thread so the
                    // parser's DEC 2026 timeout can wake independently. Bound
                    // the handoff and recycle buffers: under output flood this
                    // applies backpressure instead of retaining an unbounded
                    // queue of fresh 64 KiB allocations.
                    let (raw_tx, raw_rx) =
                        crossbeam_channel::bounded::<Option<Vec<u8>>>(PTY_PUMP_QUEUE_DEPTH);
                    let (recycle_tx, recycle_rx) =
                        std::sync::mpsc::sync_channel::<Vec<u8>>(PTY_PUMP_QUEUE_DEPTH + 1);
                    {
                        let pump_stop = stop.clone();
                        let pump_drain_output = drain_output.clone();
                        let pump_recycle_tx = recycle_tx.clone();
                        let pump_progress = Arc::clone(&reader_progress);
                        let pump_ready_tx = reader_ready_tx.clone();
                        if let Err(error) = std::thread::Builder::new()
                            .name("kettle-pty-pump".into())
                            .spawn(move || {
                                // This proves the worker can receive the Unix
                                // startup guard. It deliberately does not claim
                                // a read is pending: the guard below is what
                                // preserves output across that scheduling gap.
                                if pump_ready_tx.send(Ok(())).is_err() {
                                    return;
                                }
                                #[cfg(unix)]
                                let (mut startup_slave_guard, mut lifecycle_watcher) =
                                    match startup_slave_rx.recv() {
                                        Ok((slave, watcher)) => (Some(slave), watcher),
                                        // `spawn_command` failed, so there is
                                        // no child and the constructor dropped
                                        // its channel without handing us a
                                        // descriptor to service.
                                        Err(_) => return,
                                    };
                                #[cfg(unix)]
                                let mut child_exit_deadline = None;
                                let mut drain_buffer = None;
                                loop {
                                    if pump_stop.load(Ordering::Relaxed) {
                                        break;
                                    }
                                    #[cfg(unix)]
                                    {
                                        let lifecycle_wake =
                                            if let Some(deadline) = child_exit_deadline {
                                                wait_for_master_until(reader_poll_fd, deadline).map(
                                                    |readable| {
                                                        readable.then_some(UnixStartupWake::Output)
                                                    },
                                                )
                                            } else {
                                                lifecycle_watcher.wait(reader_poll_fd, None)
                                            };
                                        let mut record_child_exit = || {
                                            // The direct child is gone, but a
                                            // daemonized descendant may still
                                            // own the slave. Record the bounded
                                            // drain window without consuming
                                            // the wait status. The caller owns
                                            // when Kettle's retained startup
                                            // slave is released: a simultaneous
                                            // readable event must be read first
                                            // or macOS discards that final tail.
                                            let observed = std::time::Instant::now();
                                            if let Ok(mut slot) = reader_child_exit_at.lock()
                                                && slot.is_none()
                                            {
                                                *slot = Some(observed);
                                            }
                                            child_exit_deadline
                                                .get_or_insert(observed + PTY_CHILD_EXIT_EOF_TIMEOUT);
                                        };
                                        match lifecycle_wake {
                                            Ok(Some(UnixStartupWake::Output)) => {}
                                            Ok(Some(UnixStartupWake::OutputAndChildExit)) => {
                                                // Record the process event, but
                                                // keep the retained slave until
                                                // the readable bytes below have
                                                // actually been consumed.
                                                record_child_exit();
                                            }
                                            Ok(Some(UnixStartupWake::ChildExit)) => {
                                                record_child_exit();
                                                startup_slave_guard.take();
                                                continue;
                                            }
                                            Ok(None) => {
                                                // EOF never arrived after the
                                                // direct child exited. Every
                                                // chunk read before this
                                                // deadline is already ordered
                                                // ahead of the marker; stop
                                                // waiting on an out-of-scope
                                                // slave holder.
                                                pump_progress
                                                    .set_status(PtyReadStatus::EofTimeout);
                                                if !pump_drain_output.load(Ordering::Acquire) {
                                                    let _ = raw_tx.send(None);
                                                }
                                                break;
                                            }
                                            Err(error) => {
                                                // This watcher is what bounds a
                                                // slave retained by a
                                                // descendant; falling back to a
                                                // blocking read would recreate
                                                // the permanent pane hang. Fail
                                                // the reader explicitly instead.
                                                log::error!(
                                                    "PTY lifecycle watcher failed: {error}"
                                                );
                                                startup_slave_guard.take();
                                                pump_progress.set_status(PtyReadStatus::Failed);
                                                if !pump_drain_output.load(Ordering::Acquire) {
                                                    let _ = raw_tx.send(None);
                                                }
                                                break;
                                            }
                                        }
                                    }
                                    let mut buffer = drain_buffer
                                        .take()
                                        .or_else(|| recycle_rx.try_recv().ok())
                                        .unwrap_or_else(|| vec![0; PTY_READ_BUFFER_BYTES]);
                                    buffer.resize(PTY_READ_BUFFER_BYTES, 0);
                                    match reader.read(&mut buffer) {
                                        Err(error)
                                            if error.kind() == std::io::ErrorKind::WouldBlock =>
                                        {
                                            #[cfg(unix)]
                                            {
                                                let mut poll_fd = libc::pollfd {
                                                    fd: reader_poll_fd,
                                                    events: libc::POLLIN,
                                                    revents: 0,
                                                };
                                                // Bound teardown observation
                                                // without spinning while the
                                                // exec writer arbiter has the
                                                // shared master description in
                                                // nonblocking mode.
                                                let _ = unsafe { libc::poll(&mut poll_fd, 1, 250) };
                                            }
                                            #[cfg(not(unix))]
                                            std::thread::sleep(std::time::Duration::from_millis(1));
                                            let _ = pump_recycle_tx.try_send(buffer);
                                            continue;
                                        }
                                        Err(error)
                                            if error.kind() == std::io::ErrorKind::Interrupted =>
                                        {
                                            let _ = pump_recycle_tx.try_send(buffer);
                                            continue;
                                        }
                                        Ok(0) => {
                                            pump_progress.set_status(PtyReadStatus::Eof);
                                            if !pump_drain_output.load(Ordering::Acquire) {
                                                let _ = raw_tx.send(None);
                                            }
                                            break;
                                        }
                                        Err(error) => {
                                            let status = pty_read_error_status(&error);
                                            if status == PtyReadStatus::Failed {
                                                log::error!("PTY output read failed: {error}");
                                            }
                                            pump_progress.set_status(status);
                                            if !pump_drain_output.load(Ordering::Acquire) {
                                                let _ = raw_tx.send(None);
                                            }
                                            break;
                                        }
                                        Ok(n) => {
                                            #[cfg(unix)]
                                            startup_slave_guard.take();
                                            buffer.truncate(n);
                                            // Publish activity before this chunk can block behind
                                            // either the parser queue or a lossless raw-output
                                            // subscriber. ConPTY completion must not treat that
                                            // hidden work as a quiet transport.
                                            pump_progress.mark_chunk_read();
                                            match forward_pty_buffer_or_drain(
                                                &raw_tx,
                                                &pump_drain_output,
                                                buffer,
                                            ) {
                                                PtyPumpSend::Forwarded => {}
                                                PtyPumpSend::Drain(buffer) => {
                                                    pump_progress.mark_chunk_handled();
                                                    drain_buffer = Some(buffer);
                                                }
                                                PtyPumpSend::Disconnected => {
                                                    pump_progress.mark_chunk_handled();
                                                    break;
                                                }
                                            }
                                        }
                                    }
                                }
                            })
                        {
                            // Thread exhaustion must be observable. The
                            // reader cannot make progress without this blocking
                            // pump, so report the cause and close the pane
                            // through the normal exit event instead of silently
                            // waiting on a channel with no sender.
                            log::error!("failed to spawn PTY pump thread: {error}");
                            let _ = reader_ready_tx.send(Err(error.to_string()));
                            reader_progress.set_status(PtyReadStatus::Failed);
                            proxy.send_event_exit();
                            return;
                        }
                        drop(reader_ready_tx);
                    }
                    let mut image_pruner = ImageHistoryPruner::default();
                    let mut deferred_graphics = DeferredGraphicsJournal::new();
                    loop {
                        // Bail out after the detached reaper completes the
                        // platform close and publishes `stop`.
                        if stop.load(Ordering::Relaxed) {
                            break;
                        }
                        let received = {
                            let mut apply_sync_graphics =
                                |dispatch: SyncGraphicsDispatch<'_>| {
                                let finishes_sync =
                                    matches!(&dispatch, SyncGraphicsDispatch::Batch(_));
                                let generation = graphics_reflow_generation
                                    .load(std::sync::atomic::Ordering::Acquire);
                                if generation != observed_reflow_generation {
                                    clear_reflowed_regular_placements(
                                        &images,
                                        &relatives,
                                        &inactive_graphics,
                                    );
                                    extractor.clear_reflowed_regular_placements();
                                    observed_reflow_generation = generation;
                                }
                                let mut sync_graphics = SyncGraphicsContext {
                                    active_alternate: &mut active_alternate,
                                    deferred: &mut deferred_graphics,
                                    registries: GraphicsRegistries {
                                        inactive: &inactive_graphics,
                                        images: &images,
                                        virtuals: &virtuals,
                                        anims: &anims,
                                        relatives: &relatives,
                                    },
                                    actions: GraphicsActionContext {
                                        images: &images,
                                        virtuals: &virtuals,
                                        anims: &anims,
                                        relatives: &relatives,
                                        geometry: &shared_geometry,
                                    },
                                    extractor: &mut extractor,
                                };
                                apply_sync_dispatch(dispatch, &mut sync_graphics);
                                if finishes_sync {
                                    finish_deferred_sync(&mut sync_graphics);
                                }
                            };
                            let mut sync_flush = SyncFlushContext {
                                term: &term,
                                images: &images,
                                graphics_gate: &graphics_gate,
                                image_pruner: &mut image_pruner,
                                on_graphics: &mut apply_sync_graphics,
                                out_gen: &out_gen_reader,
                                output_wake: &output_wake,
                            };
                            receive_pty_chunk(&mut processor, &raw_rx, &mut sync_flush)
                        };
                        match received {
                            None => {
                                proxy.send_event_exit();
                                break;
                            }
                            Some(buffer) => {
                                if stop.load(Ordering::Relaxed) {
                                    break;
                                }
                                // Evaluated once per PTY read. If logging starts
                                // while a bounded control string is in flight,
                                // the extractor publishes the complete sequence
                                // when its terminator arrives instead of a
                                // malformed suffix.
                                let tap_raw = log_active
                                    .load(std::sync::atomic::Ordering::Relaxed)
                                    || output_tx.is_some();
                                extractor.set_raw_tap(tap_raw);
                                extractor.feed_with(&buffer, |extractor, chunk| {
                                    let mut publish_raw = |bytes: &[u8]| {
                                        // Raw consumers receive the complete PTY stream except
                                        // Kettle's private completion metadata. A full disk or
                                        // slow subscriber must not corrupt parser ordering.
                                        if log_active.load(Ordering::Relaxed)
                                            && let Ok(mut guard) = session_log.lock()
                                            && let Some(writer) = guard.as_mut()
                                        {
                                            let strip = log_strip_ansi
                                                .lock()
                                                .map(|value| *value)
                                                .unwrap_or(false);
                                            let generation =
                                                log_generation.load(Ordering::Acquire);
                                            let filtered = session_log_filter.filter(
                                                bytes,
                                                generation,
                                                strip,
                                            );
                                            if writer.try_write(filtered).is_err() {
                                                log_generation.fetch_add(1, Ordering::AcqRel);
                                                log_active.store(false, Ordering::Release);
                                            }
                                        }
                                        if let Some(tx) = &output_tx {
                                            tx.send(bytes.to_vec());
                                        }
                                    };
                                    let chunk = match chunk {
                                        Chunk::Raw(bytes) => {
                                            publish_raw(&bytes);
                                            return;
                                        }
                                        Chunk::Pass(bytes) => {
                                            publish_raw(&bytes);
                                            Chunk::Pass(bytes)
                                        }
                                        chunk => chunk,
                                    };
                                    let graphics_related = chunk_needs_graphics_gate(&chunk);
                                    let _graphics_guard = graphics_related.then(|| {
                                        graphics_gate
                                            .lock()
                                            .unwrap_or_else(|poisoned| poisoned.into_inner())
                                    });
                                    if graphics_related {
                                        let generation = graphics_reflow_generation
                                            .load(std::sync::atomic::Ordering::Acquire);
                                        if generation != observed_reflow_generation {
                                            clear_reflowed_regular_placements(
                                                &images,
                                                &relatives,
                                                &inactive_graphics,
                                            );
                                            extractor.clear_reflowed_regular_placements();
                                            observed_reflow_generation = generation;
                                        }
                                    }
                                    match chunk {
                                        Chunk::Pass(bytes) | Chunk::Terminal(bytes) => {
                                            let mut sync_graphics = SyncGraphicsContext {
                                                active_alternate: &mut active_alternate,
                                                deferred: &mut deferred_graphics,
                                                registries: GraphicsRegistries {
                                                    inactive: &inactive_graphics,
                                                    images: &images,
                                                    virtuals: &virtuals,
                                                    anims: &anims,
                                                    relatives: &relatives,
                                                },
                                                actions: GraphicsActionContext {
                                                    images: &images,
                                                    virtuals: &virtuals,
                                                    anims: &anims,
                                                    relatives: &relatives,
                                                    geometry: &shared_geometry,
                                                },
                                                extractor,
                                            };
                                            advance_terminal_bytes(
                                                &mut processor,
                                                &term,
                                                &bytes,
                                                &mut sync_graphics,
                                            );
                                        }
                                        Chunk::DeferredGraphics(graphics) => {
                                            deferred_graphics.defer(&mut processor, graphics);
                                        }
                                        Chunk::Image(placed) => {
                                            if let Some(batch) = place_image(
                                                &term,
                                                &images,
                                                &shared_geometry,
                                                &mut processor,
                                                placed,
                                            ) {
                                                apply_graphics_batch(
                                                    batch,
                                                    &mut active_alternate,
                                                    GraphicsRegistries {
                                                        inactive: &inactive_graphics,
                                                        images: &images,
                                                        virtuals: &virtuals,
                                                        anims: &anims,
                                                        relatives: &relatives,
                                                    },
                                                    extractor,
                                                );
                                            }
                                        }
                                        Chunk::DeleteImages(delete) => {
                                            let (
                                                delete_geometry,
                                                placeholder_cells,
                                                render_geometry,
                                            ) = {
                                                let (
                                                    screen_top,
                                                    screen_lines,
                                                    cursor_abs_line,
                                                    cursor_col,
                                                    cells,
                                                ) = term.lock().map_or(
                                                        (0, 1, 0, 0, Vec::new()),
                                                        |t| {
                                                            let grid = t.grid();
                                                            let screen_top = stable_grid_line_id(
                                                                grid.history_origin(),
                                                                grid.history_size(),
                                                                0,
                                                            );
                                                            let cursor = grid.cursor.point;
                                                            (
                                                                screen_top,
                                                                grid.screen_lines(),
                                                                stable_grid_line_id(
                                                                    grid.history_origin(),
                                                                    grid.history_size(),
                                                                    cursor.line.0,
                                                                ),
                                                                cursor.column.0,
                                                                Terminal::placeholder_cells_from_term(
                                                                    &t,
                                                                ),
                                                            )
                                                        },
                                                    );
                                                let render_geometry = shared_geometry
                                                    .lock()
                                                    .map(|geometry| geometry.geometry)
                                                    .unwrap_or_else(|_| {
                                                        PtyGeometry::new(1, 1, 1, 1)
                                                    });
                                                (
                                                    KittyDeleteGeometry {
                                                        screen_top,
                                                        screen_lines,
                                                        cursor_abs_line,
                                                        cursor_col,
                                                    },
                                                    cells,
                                                    render_geometry,
                                                )
                                            };

                                            // Resolve relative-placement origins before mutating
                                            // any registry. Physical selectors use the same
                                            // render-time parent chain as Terminal::relative_tiles.
                                            let image_snapshot =
                                                images.lock().map(|v| v.clone()).unwrap_or_default();
                                            let relative_snapshot = relatives
                                                .lock()
                                                .map(|v| v.clone())
                                                .unwrap_or_default();
                                            let mut origins =
                                                std::collections::HashMap::<u32, (u64, usize)>::new();
                                            let mut note_origin =
                                                |id: u32, abs: u64, col: usize| {
                                                    origins
                                                        .entry(id)
                                                        .and_modify(|origin| {
                                                            origin.0 = origin.0.min(abs);
                                                            origin.1 = origin.1.min(col);
                                                        })
                                                        .or_insert((abs, col));
                                                };
                                            for placement in &image_snapshot {
                                                if let Some(id) = placement.id {
                                                    note_origin(
                                                        id,
                                                        placement.abs_line,
                                                        placement.col,
                                                    );
                                                }
                                            }
                                            for (abs, col, resolved) in &placeholder_cells {
                                                note_origin(resolved.image_id, *abs, *col);
                                            }
                                            let relative_chains = relative_snapshot
                                                .iter()
                                                .map(|(&(id, _), entry)| {
                                                    (id, (entry.parent_img, entry.h, entry.v))
                                                })
                                                .collect::<std::collections::HashMap<_, _>>();
                                            let relative_positions = relative_snapshot
                                                .iter()
                                                .filter_map(|(&(id, placement_id), entry)| {
                                                    let (parent_abs, parent_col) = resolve_chain(
                                                        entry.parent_img,
                                                        &relative_chains,
                                                        &origins,
                                                        8,
                                                    )?;
                                                    let (abs_line, col) = relative_origin(
                                                        parent_abs,
                                                        parent_col,
                                                        entry.h,
                                                        entry.v,
                                                    );
                                                    let resolved = resolve_kitty_placement(
                                                        &entry.img,
                                                        entry.params,
                                                        render_geometry,
                                                    )?;
                                                    Some((
                                                        (id, placement_id),
                                                        Placement {
                                                            abs_line,
                                                            col,
                                                            cell_cols: resolved.cell_cols,
                                                            cell_rows: resolved.cell_rows,
                                                            x_offset_cells: resolved.x_offset_cells,
                                                            y_offset_cells: resolved.y_offset_cells,
                                                            display_cols: resolved.display_cols,
                                                            display_rows: resolved.display_rows,
                                                            img: entry.img.clone(),
                                                            source_rect: resolved.source_rect,
                                                            source_crop: None,
                                                            id: Some(id),
                                                            placement_id,
                                                            kitty_params: Some(entry.params),
                                                            z: entry.z,
                                                        },
                                                    ))
                                                })
                                                .collect::<std::collections::HashMap<_, _>>();

                                            let mut removed_keys =
                                                std::collections::HashSet::<PlacementKey>::new();
                                            let mut removed_ids =
                                                std::collections::HashSet::<u32>::new();
                                            if let Ok(mut placements) = images.lock() {
                                                placements.retain(|placement| {
                                                    if kitty_delete_matches_placement(
                                                        &delete,
                                                        placement,
                                                        delete_geometry,
                                                    ) {
                                                        if let Some(image_id) = placement.id {
                                                            removed_ids.insert(image_id);
                                                            removed_keys.insert(PlacementKey {
                                                                image_id,
                                                                placement_id: placement
                                                                    .placement_id,
                                                            });
                                                        }
                                                        false
                                                    } else {
                                                        true
                                                    }
                                                });
                                            }
                                            if let Ok(mut virtual_placements) = virtuals.lock() {
                                                virtual_placements.retain(
                                                    |&(image_id, placement_id), _| {
                                                        if kitty_delete_matches_virtual(
                                                            &delete,
                                                            image_id,
                                                            placement_id,
                                                        ) {
                                                            removed_ids.insert(image_id);
                                                            removed_keys.insert(PlacementKey {
                                                                image_id,
                                                                placement_id,
                                                            });
                                                            false
                                                        } else {
                                                            true
                                                        }
                                                    },
                                                );
                                            }
                                            if let Ok(mut relative_placements) = relatives.lock() {
                                                relative_placements.retain(
                                                    |&(image_id, placement_id), _| {
                                                        let matched = relative_positions
                                                            .get(&(image_id, placement_id))
                                                            .is_some_and(|placement| {
                                                                kitty_delete_matches_placement(
                                                                    &delete,
                                                                    placement,
                                                                    delete_geometry,
                                                                )
                                                            })
                                                            || kitty_delete_matches_virtual(
                                                                &delete,
                                                                image_id,
                                                                placement_id,
                                                            );
                                                        if matched {
                                                            removed_ids.insert(image_id);
                                                            removed_keys.insert(PlacementKey {
                                                                image_id,
                                                                placement_id,
                                                            });
                                                            false
                                                        } else {
                                                            true
                                                        }
                                                    },
                                                );

                                                // A relative placement cannot survive deletion of
                                                // its concrete parent. Repeat to cover chains.
                                                loop {
                                                    let before = relative_placements.len();
                                                    relative_placements.retain(
                                                        |&(image_id, placement_id), entry| {
                                                            let parent_removed = removed_keys
                                                                .iter()
                                                                .any(|key| {
                                                                    key.image_id
                                                                        == entry.parent_img
                                                                        && (entry
                                                                            .parent_placement
                                                                            == 0
                                                                            || key.placement_id
                                                                                == entry
                                                                                    .parent_placement)
                                                                });
                                                            if parent_removed {
                                                                removed_ids.insert(image_id);
                                                                removed_keys.insert(PlacementKey {
                                                                    image_id,
                                                                    placement_id,
                                                                });
                                                                false
                                                            } else {
                                                                true
                                                            }
                                                        },
                                                    );
                                                    if relative_placements.len() == before {
                                                        break;
                                                    }
                                                }
                                            }

                                            let mut freed_ids = Vec::new();
                                            if delete.free_data {
                                                removed_ids.extend(
                                                    delete.free_candidates.iter().copied(),
                                                );
                                                let referenced = {
                                                    let mut ids =
                                                        std::collections::HashSet::<u32>::new();
                                                    if let Ok(placements) = images.lock() {
                                                        ids.extend(
                                                            placements
                                                                .iter()
                                                                .filter_map(|placement| {
                                                                    placement.id
                                                                }),
                                                        );
                                                    }
                                                    if let Ok(virtual_placements) = virtuals.lock() {
                                                        ids.extend(
                                                            virtual_placements
                                                                .keys()
                                                                .map(|&(id, _)| id),
                                                        );
                                                    }
                                                    if let Ok(relative_placements) = relatives.lock()
                                                    {
                                                        ids.extend(
                                                            relative_placements
                                                                .keys()
                                                                .map(|&(id, _)| id),
                                                        );
                                                    }
                                                    ids
                                                };
                                                freed_ids = kitty_delete_freed_ids(
                                                    &delete,
                                                    &removed_ids,
                                                    &referenced,
                                                );
                                                if let Ok(mut animations) = anims.lock() {
                                                    for id in &freed_ids {
                                                        animations.remove(id);
                                                    }
                                                }
                                            }
                                            let removed_keys =
                                                removed_keys.into_iter().collect::<Vec<_>>();
                                            extractor.apply_kitty_delete_result(
                                                &removed_keys,
                                                &freed_ids,
                                            );
                                        }
                                        Chunk::RelativePlacement {
                                            id,
                                            placement,
                                            img,
                                            parent_img,
                                            parent_placement,
                                            h,
                                            v,
                                            z,
                                            params,
                                        } => {
                                            if let Ok(mut rm) = relatives.lock() {
                                                let key = (id, placement);
                                                let limit =
                                                    kettle_vt::GraphicsLimits::default().placements;
                                                if rm.contains_key(&key) || rm.len() < limit {
                                                    rm.insert(
                                                        key,
                                                        RelEntry {
                                                            img,
                                                            parent_img,
                                                            parent_placement,
                                                            h,
                                                            v,
                                                            z,
                                                            params,
                                                        },
                                                    );
                                                }
                                            }
                                        }
                                        Chunk::VirtualImage {
                                            id,
                                            placement,
                                            img,
                                            cols,
                                            rows,
                                            z,
                                        } => {
                                            if let Ok(mut vm) = virtuals.lock() {
                                                let limit =
                                                    kettle_vt::GraphicsLimits::default().placements;
                                                let key = (id, placement);
                                                if vm.contains_key(&key) || vm.len() < limit {
                                                    vm.insert(
                                                        key,
                                                        VirtualEntry {
                                                            img,
                                                            placement_id: placement,
                                                            cols,
                                                            rows,
                                                            z,
                                                        },
                                                    );
                                                }
                                            }
                                        }
                                        Chunk::Animation {
                                            id,
                                            imgs,
                                            gaps,
                                            state,
                                        } => {
                                            if let Ok(mut am) = anims.lock() {
                                                // An empty/single-image, not-
                                                // running snapshot = cleared.
                                                if imgs.len() <= 1 && !state.running {
                                                    am.remove(&id);
                                                } else {
                                                    // Keep the clock unless the
                                                    // run state flipped.
                                                    let started = match am.get(&id) {
                                                        Some(p)
                                                            if p.state.running == state.running =>
                                                        {
                                                            p.started
                                                        }
                                                        _ => std::time::Instant::now(),
                                                    };
                                                    let limits =
                                                        kettle_vt::GraphicsLimits::default();
                                                    let bytes =
                                                        imgs.iter().try_fold(0usize, |n, img| {
                                                            n.checked_add(img.byte_len())
                                                        });
                                                    if (am.contains_key(&id)
                                                        || am.len() < limits.placements)
                                                        && imgs.len()
                                                            <= limits
                                                                .animation_frames
                                                                .saturating_add(1)
                                                        && limits
                                                            .animation_bytes
                                                            .checked_add(limits.image_bytes)
                                                            .zip(bytes)
                                                            .is_some_and(|(cap, n)| n <= cap)
                                                    {
                                                        am.insert(
                                                            id,
                                                            AnimEntry {
                                                                imgs,
                                                                gaps,
                                                                state,
                                                                started,
                                                            },
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                        Chunk::Prompt(PromptKind::PromptStart) => {
                                            // A nested/local shell starts its completion
                                            // generation at one. PromptStart is an ordered PTY
                                            // lifecycle boundary, so retire the prior shell's
                                            // generation before accepting the new prompt's
                                            // completion stream.
                                            reset_completion_session(&completion_cell);
                                            if let Ok(t) = term.lock()
                                                && !t.mode().contains(
                                                    alacritty_terminal::term::TermMode::ALT_SCREEN,
                                                )
                                            {
                                                    // Use the application's writing cursor, not
                                                    // RenderableContent's cursor (which becomes the
                                                    // user-controlled vi cursor while vi mode is
                                                    // active). `history_origin` keeps this identity
                                                    // stable after a bounded history ring wraps.
                                                    let grid = t.grid();
                                                    let row_id = stable_grid_line_id(
                                                        grid.history_origin(),
                                                        grid.history_size(),
                                                        grid.cursor.point.line.0,
                                                    );
                                                    if let Ok(mut m) = prompts.lock() {
                                                        // Retire marks whose rows were irreversibly
                                                        // evicted or reset before appending.
                                                        m.retain(|mark| {
                                                            *mark >= grid.history_origin()
                                                        });
                                                        // O(1)
                                                        // bounded ring push (dedup +
                                                        // pop_front trim) — see
                                                        // push_prompt_mark.
                                                        push_prompt_mark(&mut m, row_id);
                                                    }
                                            }
                                        }
                                        // Terminator parity (command_notify.py):
                                        // OSC 133 OutputStart (C) marks the moment the
                                        // shell handed control to a user command. Record
                                        // the timestamp so the matching CommandEnd (D)
                                        // can compute the elapsed duration.
                                        Chunk::Prompt(PromptKind::OutputStart) => {
                                            if let Ok(mut completion) = completion_cell.lock() {
                                                completion.list = None;
                                                completion.hide_after = None;
                                            }
                                            if let Ok(mut t) = output_started_at.lock() {
                                                *t = Some(std::time::Instant::now());
                                            }
                                            // v2.20.0 (`shell_idle`): the pane has a
                                            // REAL OutputStart source — prompt marks
                                            // alone never authorize a close-confirm
                                            // skip (an A/B/D-only integration would
                                            // otherwise look permanently idle).
                                            output_start_seen
                                                .store(true, std::sync::atomic::Ordering::Relaxed);
                                        }
                                        // OSC 133 CommandEnd (D). Pop the
                                        // most-recent OutputStart timestamp, compute
                                        // the elapsed duration, push a CommandFinished
                                        // event for the App to drain. Bounded queue at
                                        // 32 entries — a runaway / hostile shell that
                                        // spams CommandEnd would otherwise grow the
                                        // Vec without bound.
                                        Chunk::Prompt(PromptKind::CommandEnd(code)) => {
                                            let started = output_started_at
                                                .lock()
                                                .ok()
                                                .and_then(|mut t| t.take());
                                            if let Some(started) = started
                                                && let Ok(mut q) = command_finished.lock()
                                            {
                                                if q.len() >= 32 {
                                                    let d = q.len() - 31;
                                                    q.drain(0..d);
                                                }
                                                q.push(CommandFinished {
                                                    duration: started.elapsed(),
                                                    exit_code: code,
                                                });
                                            }
                                        }
                                        // v2.20.0 (review fix): B = "end of prompt /
                                        // input start" — emitted via PS1/prompt AFTER
                                        // every PROMPT_COMMAND segment ran, so it
                                        // definitively means the shell is back at a
                                        // prompt. Clearing here un-sticks
                                        // `output_started_at` when a user's
                                        // pre-existing PROMPT_COMMAND echoes through
                                        // the bash DEBUG trap (which fires a stray C
                                        // after our D), which otherwise left the pane
                                        // permanently "running" and made the
                                        // prompt-aware close-confirm skip inert. No
                                        // CommandFinished is pushed (B is not a
                                        // command end).
                                        Chunk::Prompt(PromptKind::CommandStart) => {
                                            if let Ok(mut completion) = completion_cell.lock() {
                                                completion.list = None;
                                                completion.hide_after = None;
                                            }
                                            if let Ok(mut t) = output_started_at.lock() {
                                                *t = None;
                                            }
                                        }
                                        Chunk::Cwd(path) => {
                                            if let Ok(mut c) = cwd_cell.lock() {
                                                *c = Some(path);
                                            }
                                            // v2.29.1: a real shell-reported cwd —
                                            // the OSC cwd now outranks the native poll.
                                            osc_cwd_seen
                                                .store(true, std::sync::atomic::Ordering::Relaxed);
                                        }
                                        // OSC 9;4 taskbar progress.
                                        // Record the latest; the App polls it
                                        // and drives the OS taskbar indicator.
                                        Chunk::Progress(p) => {
                                            if let Ok(mut g) = progress_cell.lock() {
                                                *g = Some(p);
                                            }
                                        }
                                        Chunk::Notification { title, body } => {
                                            if let Ok(mut q) = protocol_notifications.lock() {
                                                if q.len() >= 32 {
                                                    let d = q.len() - 31;
                                                    q.drain(0..d);
                                                }
                                                q.push(ProtocolNotification { title, body });
                                            }
                                        }
                                        Chunk::Completion(update) => {
                                            apply_completion_update(&completion_cell, update);
                                        }
                                        Chunk::Raw(_) => {
                                            // Published and returned above. A
                                            // future refactor that routes one
                                            // here must still degrade to an
                                            // ignored duplicate, not take a
                                            // debug build's pane down.
                                        }
                                    }
                                });
                                image_pruner.prune_if_changed(&term, &images);
                                let _ = recycle_tx.try_send(buffer);
                                // Publish every grid and parser-sidechannel
                                // mutation through one generation-ordered,
                                // per-pane-gated wake. Graphics, progress, and
                                // protocol notifications are polled during the
                                // resulting redraw; waking them independently
                                // here would bypass hidden-window quiescence
                                // and visible flood coalescing.
                                publish_output_if_ready(
                                    &processor,
                                    &out_gen_reader,
                                    &output_wake,
                                );
                                reader_progress.mark_chunk_handled();
                            }
                        }
                    }
                })?
        };

        reader_ready_rx
            .recv()
            .context("PTY reader stopped before reporting readiness")?
            .map_err(anyhow::Error::msg)?;
        let child = pair.slave.spawn_command(cmd)?;
        // No fallible terminal setup remains after the child starts, but keep
        // the guard armed across the final value construction so an unwind
        // cannot strand a running child that no returned Terminal owns.
        let mut spawned = SpawnedChildGuard::arm(child.clone_killer());
        let child_pid = child.process_id();
        #[cfg(unix)]
        {
            let child_pid =
                child_pid.context("Unix PTY child has no process id for startup guarding")?;
            let lifecycle_watcher = UnixPtyWatcher::new(child_pid, reader_poll_fd);
            startup_slave_tx
                .send((pair.slave, lifecycle_watcher))
                .map_err(|_| anyhow::anyhow!("PTY pump stopped before accepting startup guard"))?;
        }
        #[cfg(not(unix))]
        drop(pair.slave);
        #[cfg(windows)]
        if capabilities.observe_child_exit {
            spawn_windows_child_exit_observer(
                child.as_ref(),
                Arc::clone(&direct_child_exit_at),
                Arc::clone(&lifecycle_pending),
                waker.clone(),
            )
            .context("spawn ConPTY child-exit observer")?;
        }

        // The value below takes ownership of the child and its `Drop` runs the
        // reaper. Disarm only at that handoff.
        spawned.disarm();

        Ok(Terminal {
            term,
            term_config: tconf,
            scrollback_line_limit: scrollback,
            scrollback_byte_limit: scrollback_bytes,
            master: Some(pair.master),
            #[cfg(windows)]
            pty_close: None,
            writer: Arc::new(Mutex::new(writer)),
            #[cfg(unix)]
            stdin_lease_phase: Arc::new(Mutex::new(PtyStdinLeasePhase::Available)),
            child_pid,
            child: Arc::new(Mutex::new(child)),
            reader_thread: Some(reader_thread),
            pty_read_progress,
            direct_child_exit_at,
            lifecycle_pending,
            stop,
            drain_output,
            cols,
            rows,
            images,
            virtuals,
            anims,
            relatives,
            inactive_graphics,
            graphics_gate,
            graphics_reflow_generation,
            prompts,
            output_started_at,
            command_finished,
            protocol_notifications,
            completion: completion_cell,
            cwd: cwd_cell,
            native_cwd: native_cwd_cell,
            osc_cwd_seen: osc_cwd_seen_for_struct,
            progress: progress_cell,
            argv: argv.to_vec(),
            session_log: session_log_for_struct,
            log_strip_ansi: log_strip_ansi_for_struct,
            log_active: log_active_for_struct,
            log_generation: log_generation_for_struct,
            log_failure_reported: AtomicBool::new(false),
            log_waker: waker,
            output_start_seen: output_start_seen_for_struct,
            geometry: shared_geometry,
            applied_pty_geometry: geometry,
            output_wake,
            event_overflowed,
            osc52_copy_allowed,
            out_gen,
        })
    }

    /// C4 (multi-window): monotone counter of PTY reads this pane's reader
    /// thread has processed. A UI that recorded the value at its last paint
    /// can answer "any output since?" without draining the event channel
    /// (plain text emits no `TermEvent`). Bumped with `Release` before the
    /// wakeup fires; read with `Acquire`.
    pub fn output_generation(&self) -> u64 {
        self.out_gen.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Update whether DA1 may advertise OSC 52 clipboard writes.
    ///
    /// The event proxy reads this atomically while parsing a DA request, so a
    /// live config reload takes effect without restarting the pane.
    pub fn set_osc52_copy_allowed(&self, allowed: bool) {
        self.osc52_copy_allowed.store(allowed, Ordering::Release);
    }

    /// Update Kettle's modified-Enter fallback before keyboard negotiation.
    ///
    /// Applying the engine option updates `TermMode` immediately without
    /// replacing the level selected by the focused application.
    pub fn set_unnegotiated_modified_enter(&mut self, enabled: bool) {
        if self.term_config.unnegotiated_modified_enter == enabled {
            return;
        }

        self.term_config.unnegotiated_modified_enter = enabled;
        let mut term = self
            .term
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        term.set_options(self.term_config.clone());
    }

    /// Whether the live Unix PTY line discipline is in canonical mode.
    ///
    /// Interactive TUIs switch the slave to raw/noncanonical mode, but that
    /// signal is not sufficient by itself: zsh ZLE and Bash Readline can also
    /// edit a prompt in noncanonical mode. Callers combine this snapshot with
    /// the foreground process group before encoding modified Enter, and fail
    /// closed when either observation is unavailable.
    #[cfg(unix)]
    pub fn input_is_canonical(&self) -> Result<bool> {
        let fd = self
            .master
            .as_ref()
            .context("PTY master is unavailable")?
            .as_raw_fd()
            .context("PTY master has no Unix file descriptor")?;
        let mut attrs = unsafe { std::mem::zeroed::<libc::termios>() };
        if unsafe { libc::tcgetattr(fd, &mut attrs) } != 0 {
            return Err(std::io::Error::last_os_error())
                .context("cannot read live PTY termios for key encoding");
        }
        Ok(attrs.c_lflag & libc::ICANON != 0)
    }

    /// Current Unix PTY foreground process group id.
    ///
    /// This is a single `tcgetpgrp` snapshot over the master descriptor. It
    /// does not inspect the child handle and therefore cannot wait behind the
    /// asynchronous child reaper during pane teardown.
    #[cfg(unix)]
    pub fn foreground_process_group(&self) -> Result<u32> {
        let fd = self
            .master
            .as_ref()
            .context("PTY master is unavailable")?
            .as_raw_fd()
            .context("PTY master has no Unix file descriptor")?;
        let foreground = unsafe { libc::tcgetpgrp(fd) };
        if foreground < 0 {
            return Err(std::io::Error::last_os_error())
                .context("cannot read the PTY foreground process group");
        }
        u32::try_from(foreground).context("PTY foreground process group is out of range")
    }

    /// Current command state when OSC 133 supplies enough information to know.
    pub fn shell_activity(&self) -> ShellActivity {
        let seen_prompts = self.prompts.lock().map(|p| !p.is_empty()).unwrap_or(false);
        let tracks_commands = self
            .output_start_seen
            .load(std::sync::atomic::Ordering::Relaxed);
        let running = self
            .output_started_at
            .lock()
            .ok()
            .map(|state| state.is_some());
        classify_shell_activity(seen_prompts, tracks_commands, running)
    }

    /// v2.20.0 (Ghostty `confirm-close-surface` parity): is this pane's
    /// shell sitting IDLE at a prompt? True only when shell integration has
    /// been observed (≥1 OSC 133 prompt mark) AND no command is currently
    /// running (`output_started_at` is the OutputStart→CommandEnd window —
    /// a full-screen app like vim counts as running until it exits).
    /// Without integration this is always `false`, so close-confirmation
    /// behavior is byte-identical for plain shells; a command whose
    /// CommandEnd never arrives stays "running", which errs toward asking.
    pub fn shell_idle(&self) -> bool {
        self.shell_activity() == ShellActivity::Idle
    }

    /// Install a path whose secure open/create is deferred to the persistence
    /// worker, or remove the current per-pane session log. This is the UI path:
    /// no filesystem operation or file close can run on the event loop.
    pub fn set_log_path(&self, path: Option<std::path::PathBuf>) -> std::io::Result<bool> {
        self.set_log_writer(
            path.map(|path| Box::new(LazySessionLogWriter::new(path)) as Box<dyn Write + Send>),
        )
    }

    /// Install an already-open file for callers that own file preparation, or
    /// remove the current per-pane session log. Production UI callers use
    /// [`Terminal::set_log_path`] so file preparation stays off winit.
    pub fn set_log_file(&self, file: Option<std::fs::File>) -> std::io::Result<bool> {
        self.set_log_writer(
            file.map(|file| Box::new(std::io::BufWriter::new(file)) as Box<dyn Write + Send>),
        )
    }

    fn set_log_writer(&self, writer: Option<Box<dyn Write + Send>>) -> std::io::Result<bool> {
        let mut guard = self
            .session_log
            .lock()
            .map_err(|_| std::io::Error::other("session log lock poisoned"))?;
        let was = self.log_active.load(Ordering::Acquire);
        match writer {
            Some(writer) => {
                if let Some(previous) = guard.as_mut() {
                    if !previous.try_join() {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::WouldBlock,
                            "the previous session log is still closing",
                        ));
                    }
                    *guard = None;
                }
                let mut writer = AsyncFileWriter::spawn("kettle-session-log-writer", writer)?;
                let active = Arc::clone(&self.log_active);
                let generation = Arc::clone(&self.log_generation);
                let wake = Arc::clone(&self.log_waker);
                writer.set_failure_waker(Arc::new(move || {
                    generation.fetch_add(1, Ordering::AcqRel);
                    active.store(false, Ordering::Release);
                    wake();
                }));
                self.log_failure_reported.store(false, Ordering::Release);
                *guard = Some(writer);
                self.log_generation.fetch_add(1, Ordering::AcqRel);
                self.log_active.store(true, Ordering::Release);
            }
            None => {
                self.log_active.store(false, Ordering::Release);
                self.log_generation.fetch_add(1, Ordering::AcqRel);
                self.log_failure_reported.store(false, Ordering::Release);
                if let Some(writer) = guard.as_mut() {
                    writer.request_finish();
                }
            }
        }
        Ok(was)
    }

    /// Whether a session log is currently installed.
    pub fn log_enabled(&self) -> bool {
        self.log_active.load(Ordering::Acquire)
    }

    /// Return one user-reportable failure edge from the background logger.
    pub fn take_session_log_failure(&self) -> Option<SessionLogFailure> {
        if self.log_failure_reported.load(Ordering::Acquire) {
            return None;
        }
        let mut guard = self.session_log.lock().ok()?;
        let writer = guard.as_mut()?;
        let failure = match writer.status() {
            AsyncWriterStatus::Overloaded => SessionLogFailure::Overloaded,
            AsyncWriterStatus::IoError => SessionLogFailure::IoError,
            AsyncWriterStatus::Active | AsyncWriterStatus::Finished => return None,
        };
        if self
            .log_failure_reported
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            Some(failure)
        } else {
            None
        }
    }

    /// Last working directory reported via OSC 7 (or OSC 9;9), if any. This is
    /// the authoritative shell-volunteered cwd; callers that must NOT trust an
    /// OS-derived guess (e.g. WSL split-cloning) use this directly.
    pub fn current_dir(&self) -> Option<String> {
        self.cwd.lock().ok().and_then(|c| c.clone())
    }

    /// Working directory explicitly reported by the child via OSC 7/9;9.
    ///
    /// Unlike [`current_dir`](Self::current_dir), this never exposes the
    /// launch-directory seed before the first cwd report. Use this when a
    /// caller must distinguish shell-reported state from a startup fallback.
    pub fn reported_current_dir(&self) -> Option<String> {
        reported_current_dir(
            self.osc_cwd_seen.load(std::sync::atomic::Ordering::Relaxed),
            self.current_dir(),
        )
    }

    /// v2.29.0: set the OS-derived native cwd fallback (the App's process poll
    /// writes this for native shells lacking OSC 7/9;9). `None` clears it.
    pub fn set_native_cwd(&self, dir: Option<String>) {
        if let Ok(mut c) = self.native_cwd.lock() {
            *c = dir;
        }
    }

    /// v2.29.0: the cwd to display in tab/window/pane labels.
    ///
    /// If the shell has actually REPORTED a cwd via OSC 7/9;9 (`osc_cwd_seen`),
    /// that is authoritative — return it, so a shell that volunteers its directory
    /// (including WSL, where the native Windows read is meaningless) is unaffected.
    ///
    /// Otherwise the only value in `cwd` is the pre-seeded *launch* directory,
    /// which never tracks `cd`; prefer the live OS-derived `native_cwd` poll
    /// (which does), falling back to that launch seed until the first poll lands.
    /// (v2.29.1 fix: previously this always preferred `cwd`, so the seeded launch
    /// dir shadowed the native poll and a stock Windows shell's tab stayed frozen.)
    pub fn current_dir_or_native(&self) -> Option<String> {
        if self.osc_cwd_seen.load(std::sync::atomic::Ordering::Relaxed) {
            return self.current_dir();
        }
        self.native_cwd
            .lock()
            .ok()
            .and_then(|c| c.clone())
            .or_else(|| self.current_dir())
    }

    /// Latest OSC 9;4 taskbar-progress state reported by this pane
    /// (`None` if never reported, or explicitly cleared with state 0). The
    /// App polls the focused pane's value each frame to drive the OS taskbar.
    pub fn progress(&self) -> Option<Progress> {
        self.progress.lock().ok().and_then(|g| *g)
    }

    /// Terminator parity (phase 1 of
    /// [`TERMINATOR-REMOTE-DESIGN.md`](docs/TERMINATOR-REMOTE-DESIGN.md)):
    /// PTY child PID accessor. Returns the immutable OS pid captured when the
    /// pane was spawned. `None` means the platform does not expose a pid for
    /// this Child type (the Windows fallback path).
    ///
    /// Used by the upcoming remote-session detector to root the
    /// process-tree walk. Read-only — does not consume the Child.
    pub fn child_pid(&self) -> Option<u32> {
        self.child_pid
    }

    /// Observe an exited Unix child without consuming its wait status.
    ///
    /// `kettle exec` must know when to start its bounded PTY EOF wait, but on
    /// Linux the unreaped child is also the kernel-held identity anchor that
    /// prevents its session/process-group number from being recycled while a
    /// later output deadline can still win. `waitid(..., WNOWAIT)` supplies
    /// both facts: an exit code for ordinary completion and a zombie retained
    /// until the final output/recording acknowledgement is complete.
    #[cfg(unix)]
    pub fn child_exit_code_unreaped(&self) -> io::Result<Option<u32>> {
        let pid = self
            .child
            .lock()
            .map_err(|_| io::Error::other("child handle is poisoned"))?
            .process_id()
            .ok_or_else(|| io::Error::other("PTY child has no process id"))?;
        unix_child_exit_code_unreaped(pid)
    }

    /// Start closing the ConPTY master without blocking the lifecycle thread.
    ///
    /// This is used only after the child has exited and a bounded quiet period
    /// has elapsed. The output reader deliberately remains live: older Windows
    /// versions can block `ClosePseudoConsole` until conout has been drained,
    /// and the resulting pipe close is the authoritative boundary after which
    /// headless output may be finalized without racing a late repaint.
    #[cfg(windows)]
    pub fn begin_pty_output_close(&mut self) -> io::Result<bool> {
        let Some(master) = self.master.take() else {
            return Ok(false);
        };
        let close = Arc::new(Mutex::new(Some(master)));
        let worker = Arc::clone(&close);
        let state = Arc::new(ConPtyCloseState::default());
        let worker_state = Arc::clone(&state);
        let stop = Arc::clone(&self.stop);
        match std::thread::Builder::new()
            .name("kettle-pty-output-close".into())
            .spawn(move || {
                let master = worker
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take();
                drop(master);
                worker_state.close_completed(&stop);
            }) {
            Ok(_) => {
                self.pty_close = Some(state);
                Ok(true)
            }
            Err(error) => {
                self.master = close
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take();
                Err(error)
            }
        }
    }

    /// Number of processes still alive in this command's Windows Job Object.
    /// `None` means the PTY child was not spawned with a containment scope.
    #[cfg(windows)]
    pub fn process_tree_active_processes(&self) -> io::Result<Option<u32>> {
        self.child
            .lock()
            .map_err(|_| io::Error::other("child handle is poisoned"))?
            .process_tree_active_processes()
    }

    /// Current terminal state of the blocking PTY output reader.
    pub fn pty_read_status(&self) -> PtyReadStatus {
        self.pty_read_progress.load().status
    }

    /// Source-side activity and outstanding work in the PTY reader pipeline.
    pub fn pty_read_progress(&self) -> PtyReadProgress {
        self.pty_read_progress.load()
    }

    /// When the direct child was observed exited without consuming its wait
    /// status. EOF may follow later after the PTY reader drains its tail.
    pub fn direct_child_exit_at(&self) -> Option<std::time::Instant> {
        self.direct_child_exit_at.lock().ok().and_then(|slot| *slot)
    }

    /// Consume the edge that says semantic/lifecycle work is waiting. This is
    /// deliberately independent of output generation: a quiet child exit and
    /// a hidden window still need UI policy even when no frame can be painted.
    pub fn take_lifecycle_wake(&self) -> bool {
        self.lifecycle_pending.swap(false, Ordering::AcqRel)
    }

    /// Terminator parity (`command_notify.py`): pop every
    /// `CommandFinished` event the reader thread queued since the
    /// previous call. The App drains this each tick to fire desktop
    /// notifications for long commands that completed while the
    /// window was unfocused. Empty Vec when the shell hasn't shipped
    /// OSC 133 D events (no shell integration) or no command has
    /// completed since the last drain.
    pub fn drain_command_finished_events(&self) -> Vec<CommandFinished> {
        self.command_finished
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default()
    }

    /// Pop every protocol desktop notification the reader thread queued since
    /// the previous call. Empty when no PTY program emitted OSC 9/777
    /// notification requests.
    pub fn drain_protocol_notifications(&self) -> Vec<ProtocolNotification> {
        self.protocol_notifications
            .lock()
            .map(|mut q| std::mem::take(&mut *q))
            .unwrap_or_default()
    }

    /// The completion list to draw right now, or `None` once a pending
    /// grace-hide has lapsed with no refresh from the shell.
    pub fn completion(&self) -> Option<CompletionList> {
        let now = std::time::Instant::now();
        self.completion
            .lock()
            .ok()
            .and_then(|slot| completion_visible(&slot, now).cloned())
    }

    /// Hide the current shell completion list without rewinding its protocol
    /// generation. A delayed older update must not resurrect after input.
    pub fn clear_completion(&self) {
        if let Ok(mut completion) = self.completion.lock() {
            completion.list = None;
            completion.hide_after = None;
        }
    }

    /// Keep the current list visible a moment longer because the input just
    /// sent (Tab / Shift-Tab) is expected to replace it. Clearing outright
    /// instead blinks the card off once per cycle step for the PTY round-trip.
    pub fn defer_completion_hide(&self) {
        if let Ok(mut completion) = self.completion.lock()
            && completion.list.is_some()
        {
            completion.hide_after = Some(std::time::Instant::now() + COMPLETION_HIDE_GRACE);
        }
    }

    /// Expire a grace-hidden completion or report how long the event loop must
    /// wait before checking again. Returning the redraw edge separately keeps
    /// an idle, non-blinking terminal asleep after the one erase frame.
    pub fn poll_completion_hide(
        &self,
        now: std::time::Instant,
    ) -> (bool, Option<std::time::Duration>) {
        let Ok(mut completion) = self.completion.lock() else {
            return (false, None);
        };
        poll_completion_hide_slot(&mut completion, now)
    }

    /// Live cursor-blink state. Defaults to whatever the config seeded at
    /// pane creation; programs flip it at runtime via DEC private mode 12
    /// (`CSI ?12 h` blink / `?12 l` solid) — the engine raises
    /// `TermEvent::CursorBlinkingChange` and we re-read this on next redraw
    /// so the cursor obeys the running app, not just the config.
    pub fn cursor_blinking(&self) -> bool {
        self.term
            .lock()
            .map(|t| t.cursor_style().blinking)
            .unwrap_or(false)
    }

    /// Retained OSC 133 prompt row ids in oldest-to-newest order.
    ///
    /// Stale ids are pruned against the terminal grid's monotonic history
    /// origin before returning, so callers can never treat an evicted row's
    /// reused grid coordinate as the original prompt.
    pub fn prompt_marks(&self) -> Vec<u64> {
        let Ok(term) = self.term.lock() else {
            return Vec::new();
        };
        if term
            .mode()
            .contains(alacritty_terminal::term::TermMode::ALT_SCREEN)
        {
            // Prompt ids belong to the primary grid. Never prune them against
            // the alternate grid's independent history origin.
            return Vec::new();
        }
        let grid = term.grid();
        let origin = grid.history_origin();
        let retained_end = origin
            .saturating_add(grid.history_size() as u64)
            .saturating_add(grid.screen_lines() as u64);
        self.prompts
            .lock()
            .map(|mut marks| {
                marks.retain(|mark| *mark >= origin && *mark < retained_end);
                marks.iter().copied().collect()
            })
            .unwrap_or_default()
    }

    /// Scroll to the adjacent retained OSC 133 prompt.
    ///
    /// The term and prompt ring are locked in the same order as the reader
    /// thread (`Term` then prompt ring), giving the navigation decision a
    /// consistent history-origin snapshot without a deadlock inversion.
    pub fn jump_to_prompt(&self, previous: bool) -> bool {
        let Ok(mut term) = self.term.lock() else {
            return false;
        };
        if term
            .mode()
            .contains(alacritty_terminal::term::TermMode::ALT_SCREEN)
        {
            return false;
        }
        let grid = term.grid();
        let history_origin = grid.history_origin();
        let history_size = grid.history_size();
        let screen_lines = grid.screen_lines();
        let display_offset = grid.display_offset();
        let Ok(mut marks) = self.prompts.lock() else {
            return false;
        };
        let Some(new_offset) = prompt_navigation_offset(
            &mut marks,
            history_origin,
            history_size,
            screen_lines,
            display_offset,
            previous,
        ) else {
            return false;
        };
        drop(marks);

        let delta = if new_offset >= display_offset {
            i32::try_from(new_offset - display_offset).unwrap_or(i32::MAX)
        } else {
            -i32::try_from(display_offset - new_offset).unwrap_or(i32::MAX)
        };
        if delta != 0 {
            term.scroll_display(alacritty_terminal::grid::Scroll::Delta(delta));
        }
        true
    }

    /// Image placements for this terminal (cloned cheaply; `ImageData` is
    /// `Arc`-backed). For placements whose kitty id has a registered
    /// animation, the image is swapped for the frame the playback clock
    /// selects right now, so animations play wherever the image sits.
    pub fn placements(&self) -> Vec<Placement> {
        let mut v = self.images.lock().map(|v| v.clone()).unwrap_or_default();
        if let Ok(am) = self.anims.lock()
            && !am.is_empty()
        {
            for p in &mut v {
                if let Some(id) = p.id
                    && let Some(e) = am.get(&id)
                    && let Some(frame) = e.current()
                {
                    p.img = frame.clone();
                }
            }
        }
        v
    }

    /// `true` if any registered kitty animation is currently running (so the
    /// UI knows to schedule frame-paced redraws).
    pub fn has_running_animation(&self) -> bool {
        self.anims
            .lock()
            // Require a DISPLAYABLE frame, not
            // just the running flag. With all-zero gaps current_frame never
            // advances, yet a bare `running` check kept the UI scheduling a
            // ~30fps redraw forever for an animation that can never change.
            .map(|am| {
                am.values()
                    .any(|e| e.state.running && e.gaps.iter().any(|&g| g > 0))
            })
            .unwrap_or(false)
    }

    /// Per-cell image tiles for the kitty Unicode placeholders (`U+10EEEE`)
    /// currently visible: decode each cell's `(image-id, row, column)` from
    /// its foreground color + combining diacritics, apply the left-
    /// inheritance rules over contiguous runs, and slice the referenced
    /// virtual image into one `Placement` per cell. Recomputed per frame —
    /// cheap: `ImageData` is `Arc`-backed and only the shown tiles are
    /// cropped. The placement id is decoded from the cell's underline
    /// color (used for run grouping / inheritance per the spec); a single
    /// virtual placement is stored per image id, so it also selects it.
    /// Scan the visible grid for `U+10EEEE` placeholder cells and resolve
    /// each one (image id + in-image row/col after diacritic inheritance) to
    /// its absolute line and column. Shared by placeholder + relative tiles.
    fn placeholder_cells(&self) -> Vec<(u64, usize, placeholder::ResolvedCell)> {
        let t = self
            .term
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        Self::placeholder_cells_from_term(&t)
    }

    fn placeholder_cells_from_term(
        t: &Term<EventProxy>,
    ) -> Vec<(u64, usize, placeholder::ResolvedCell)> {
        let grid = t.grid();
        let history_origin = grid.history_origin();
        let history_size = grid.history_size();
        let content = t.renderable_content();

        // Maximal same-row contiguous runs of placeholder cells.
        let mut runs: Vec<Vec<(RawCell, u64, usize)>> = Vec::new();
        let mut last: Option<(i32, i32)> = None;
        for ind in content.display_iter {
            let (cell, p) = (ind.cell, ind.point);
            if cell.c == placeholder::PLACEHOLDER {
                let contiguous = matches!(
                    last,
                    Some((r, c)) if r == p.line.0 && c + 1 == p.column.0 as i32
                );
                if !contiguous || runs.is_empty() {
                    runs.push(Vec::new());
                }
                let marks: Vec<char> = cell.zerowidth().map(|z| z.to_vec()).unwrap_or_default();
                // `runs` is non-empty here (we either just pushed an
                // empty Vec or `contiguous` held with runs already non-empty).
                // Use `if let` rather than `expect()` so the invariant can never
                // panic the PTY reader thread (panic=abort) if a future refactor
                // changes the push logic — the run is simply skipped instead.
                if let Some(run) = runs.last_mut() {
                    run.push((
                        RawCell {
                            fg: fg_id_bits(cell.fg),
                            // Underline color carries the placement id (0/absent
                            // ⇒ any placement); spec §"Unicode placeholders".
                            placement_id: cell.underline_color().map(fg_id_bits).unwrap_or(0),
                            diacritics: CellDiacritics::parse(&marks),
                        },
                        stable_grid_line_id(history_origin, history_size, p.line.0),
                        p.column.0,
                    ));
                }
                last = Some((p.line.0, p.column.0 as i32));
            } else {
                last = None;
            }
        }

        let mut out = Vec::new();
        for run in &runs {
            let cells: Vec<RawCell> = run.iter().map(|(rc, _, _)| *rc).collect();
            for (res, &(_, abs, col)) in
                placeholder::resolve_run(&cells).into_iter().zip(run.iter())
            {
                out.push((abs, col, res));
                if out.len() >= kettle_vt::GraphicsLimits::default().placements {
                    return out;
                }
            }
        }
        out
    }

    /// Owned copy of the virtual placements plus, per image id, the smallest
    /// placement id registered for it. `None` when there are none.
    ///
    /// The owned return type is the point: it makes the borrow checker — not a
    /// comment or a source guard — prove the `virtuals` lock is released before
    /// the caller touches `term`. A `MutexGuard` cannot escape through this
    /// signature, so no edit inside can leave one live for `placeholder_tiles`.
    #[allow(clippy::type_complexity)]
    fn virtuals_snapshot(&self) -> Option<(HashMap<(u32, u32), VirtualEntry>, HashMap<u32, u32>)> {
        let virtuals = self.virtuals.lock().ok()?;
        if virtuals.is_empty() {
            return None;
        }
        // A zero/omitted underline placement id selects any virtual placement
        // for the image; the smallest id is chosen so rendering and tests are
        // deterministic. Resolving that per cell rescanned every virtual, which
        // is O(cells x virtuals) — up to 256x256 scans in a single frame. One
        // pass builds the answer for every image instead.
        let mut smallest_for_image: HashMap<u32, u32> = HashMap::new();
        for (image_id, placement_id) in virtuals.keys() {
            smallest_for_image
                .entry(*image_id)
                .and_modify(|current| *current = (*current).min(*placement_id))
                .or_insert(*placement_id);
        }
        // Bounded by the protocol's placement cap, and `ImageData` is
        // `Arc`-backed, so this clone is far cheaper than the grid walk it lets
        // the empty case skip.
        Some((virtuals.clone(), smallest_for_image))
    }

    pub fn placeholder_tiles(&self) -> Vec<Placement> {
        // Snapshot the virtual placements, then read the grid — the single
        // lock-acquisition order that `relative_tiles` below already keeps.
        // `virtuals` must never be held across a `term` acquisition: the PTY
        // reader takes `term` and then `virtuals` to replay a deferred kitty
        // virtual chunk, so the opposite order deadlocks, and a child emitting
        // `CSI ? 2026 h`, a deferred virtual placement, placeholder cells and
        // `CSI ? 2026 l` while the UI paints would park both threads forever
        // and freeze the pane. `virtuals_snapshot` returns owned maps, so the
        // guard provably cannot reach the `placeholder_cells` call below.
        //
        // Snapshotting first is also what makes the common case free. Every
        // visible pane calls this every frame, and almost none of them have a
        // kitty virtual placement; `placeholder_cells` walks the whole visible
        // grid under the `term` lock, so testing `virtuals` afterwards paid for
        // a full scan of every pane on every frame to reach an empty map.
        let Some((virtuals, smallest_for_image)) = self.virtuals_snapshot() else {
            return Vec::new();
        };

        let cells = self.placeholder_cells();
        if cells.is_empty() {
            return Vec::new();
        }

        let mut out = Vec::new();
        for (abs, col, res) in cells {
            let placement_id = if res.placement_id != 0 {
                res.placement_id
            } else {
                let Some(smallest) = smallest_for_image.get(&res.image_id) else {
                    continue;
                };
                *smallest
            };
            let Some(v) = virtuals.get(&(res.image_id, placement_id)) else {
                continue;
            };
            if let Some(placement) = placeholder_tile_placement(abs, col, res, v) {
                out.push(placement);
                if out.len() >= kettle_vt::GraphicsLimits::default().placements {
                    break;
                }
            }
        }
        out
    }

    /// Placements for kitty relative placements whose parent is a visible
    /// Unicode-placeholder (virtual) image: the parent's origin is the
    /// top-left of its placeholder cells, and the child image is drawn
    /// `(h, v)` cells from there. Parents that aren't on screen this frame
    /// are skipped (the relative is simply not shown). Non-placeholder /
    /// chained parents are a later sub-item (see ROADMAP).
    pub fn relative_tiles(&self) -> Vec<Placement> {
        // Snapshot the relatives, then drop the lock before taking the
        // grid / images locks (keeps a single lock-acquisition order).
        let entries: Vec<(u32, u32, RelEntry)> = {
            let Ok(rel) = self.relatives.lock() else {
                return Vec::new();
            };
            if rel.is_empty() {
                return Vec::new();
            }
            let mut entries: Vec<_> = rel
                .iter()
                .map(|(&(image_id, placement_id), entry)| (image_id, placement_id, entry.clone()))
                .collect();
            entries.sort_by_key(|(image_id, placement_id, _)| (*image_id, *placement_id));
            entries.truncate(kettle_vt::GraphicsLimits::default().placements);
            entries
        };
        // Concrete origins: a parent is either a placeholder/virtual image
        // (top-left of its cells) or a regular placement (its abs_line/col).
        let mut origins: std::collections::HashMap<u32, (u64, usize)> =
            std::collections::HashMap::new();
        let mut note = |id: u32, abs: u64, col: usize| {
            origins
                .entry(id)
                .and_modify(|o: &mut (u64, usize)| {
                    o.0 = o.0.min(abs);
                    o.1 = o.1.min(col);
                })
                .or_insert((abs, col));
        };
        // Snapshot visible placeholder origins and exact grid/pixel geometry
        // under the same Term -> geometry order used by resize and insertion.
        // A concurrent DPI reflow therefore cannot pair pre-resize origins
        // with post-resize image-cell conversion.
        let (placeholder_cells, geometry) = {
            let term = self
                .term
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let geometry = self
                .geometry
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (Self::placeholder_cells_from_term(&term), geometry.geometry)
        };
        for (abs, col, res) in placeholder_cells {
            note(res.image_id, abs, col);
        }
        if let Ok(imgs) = self.images.lock() {
            for p in imgs.iter() {
                if let Some(id) = p.id {
                    note(id, p.abs_line, p.col);
                }
            }
        }
        // child image id -> (parent image id, h, v), for chain walking.
        let rels: std::collections::HashMap<u32, (u32, i32, i32)> = entries
            .iter()
            .map(|(image_id, _, entry)| (*image_id, (entry.parent_img, entry.h, entry.v)))
            .collect();
        let mut out = Vec::new();
        for (cimg, placement_id, e) in &entries {
            // kitty requires a chain depth of at least 8.
            let Some((pa, pc)) = resolve_chain(e.parent_img, &rels, &origins, 8) else {
                continue;
            };
            let (abs, col) = relative_origin(pa, pc, e.h, e.v);
            let Some(resolved) = resolve_kitty_placement(&e.img, e.params, geometry) else {
                continue;
            };
            out.push(Placement {
                abs_line: abs,
                col,
                cell_cols: resolved.cell_cols,
                cell_rows: resolved.cell_rows,
                x_offset_cells: resolved.x_offset_cells,
                y_offset_cells: resolved.y_offset_cells,
                display_cols: resolved.display_cols,
                display_rows: resolved.display_rows,
                img: e.img.clone(),
                source_rect: resolved.source_rect,
                source_crop: None,
                id: Some(*cimg),
                placement_id: *placement_id,
                kitty_params: Some(e.params),
                z: e.z,
            });
        }
        out
    }

    pub fn write(&self, bytes: &[u8]) {
        if let Err(error) = PtyWriter(Arc::clone(&self.writer)).write_all_checked(bytes) {
            log::error!("cannot deliver complete input to child PTY: {error:#}");
        }
    }

    /// Create the sole active writer-arbiter handle.
    ///
    /// Unix temporarily enables nonblocking status on the shared master
    /// open-file description and the PTY pump polls through `WouldBlock`.
    /// Because duplicated descriptors share those status flags, a second live
    /// handle is rejected instead of letting either drop restore flags beneath
    /// the other. Output parsing and lifecycle ownership remain with
    /// `Terminal`.
    pub fn stdin_handle(&self) -> Result<PtyStdin> {
        #[cfg(unix)]
        {
            let master = self
                .master
                .as_ref()
                .context("PTY master unavailable while creating stdin handle")?;
            let fd = master
                .as_raw_fd()
                .context("PTY master has no Unix file descriptor")?;
            let lease = UnixPtyStdinLease::acquire(fd, Arc::clone(&self.stdin_lease_phase))?;
            Ok(PtyStdin {
                writer: PtyWriter(self.writer.clone()),
                lease,
                input_state: PtyInputTail::default(),
                pending_eof: None,
            })
        }
        #[cfg(not(unix))]
        {
            Ok(PtyStdin {
                writer: PtyWriter(self.writer.clone()),
            })
        }
    }

    /// Agent-first (A1): a cloneable, `Send + Sync` handle to the
    /// PTY's write side. `Terminal` itself is not `Sync` (it owns the
    /// `Send`-only master), so a worker thread (e.g. `kettle exec`'s stdin
    /// pump) can't share the whole engine — but it CAN hold a `PtyWriter` to
    /// feed input. The handle keeps writing valid bytes even after the
    /// `Terminal` is dropped (Drop swaps the writer for a discard sink).
    pub fn writer_handle(&self) -> PtyWriter {
        PtyWriter(self.writer.clone())
    }

    pub fn resize(&mut self, cols: usize, rows: usize, cell_w: u16, cell_h: u16) -> Result<()> {
        self.resize_with_pixels(
            cols,
            rows,
            cell_w,
            cell_h,
            clamp_pty_dim(cell_w.max(1), cols),
            clamp_pty_dim(cell_h.max(1), rows),
        )
    }

    /// Resize the terminal while preserving exact total pixel geometry.
    ///
    /// The legacy integer `cell_w`/`cell_h` arguments are retained for API
    /// compatibility; exact total pixel dimensions are authoritative for the
    /// PTY and image placement. A renderer at a fractional DPI scale must
    /// round only after multiplying its fractional cell metric by the grid
    /// size. Multiplying a truncated per-cell value under-reports wide grids
    /// and can suppress a pixel-only SIGWINCH.
    pub fn resize_with_pixels(
        &mut self,
        cols: usize,
        rows: usize,
        _cell_w: u16,
        _cell_h: u16,
        pixel_width: u16,
        pixel_height: u16,
    ) -> Result<()> {
        if cols == 0 || rows == 0 {
            return Ok(());
        }
        self.try_resize_geometry(PtyGeometry::new(cols, rows, pixel_width, pixel_height))
    }

    /// Apply an edited `scrollback` / `scrollback-bytes` budget to a pane that
    /// is already running. Returns whether the effective cap moved.
    ///
    /// Deliberately not the resize rule. A resize must never lower the cap —
    /// nothing about dragging a window wider means the user wants less history,
    /// and `Grid::update_history` enforces a lowered limit by discarding the
    /// oldest rows immediately and irreversibly. Editing the setting is the
    /// opposite: it is the user saying exactly that, so a decrease is honored
    /// here and only here.
    ///
    /// Without this, the Settings overlay's two scrollback rows and any edit to
    /// the config file wrote the new value, reloaded it, and changed nothing
    /// visible — the budget was read once at spawn, so only panes opened
    /// afterwards used it.
    pub fn set_scrollback_limits(&mut self, lines: usize, bytes: usize) -> bool {
        if (self.scrollback_line_limit, self.scrollback_byte_limit) == (lines, bytes) {
            return false;
        }
        self.scrollback_line_limit = lines;
        self.scrollback_byte_limit = bytes;
        // Read the geometry and release it before touching Term: the shared
        // lock order is Term then geometry, so nesting the other way would be
        // an ABBA against the resize path.
        let geometry = self
            .geometry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .geometry;
        let history = effective_scrollback_lines(lines, bytes, geometry.columns, geometry.rows);
        if history == self.term_config.scrolling_history {
            return false;
        }
        self.term_config.scrolling_history = history;
        let options = self.term_config.clone();
        self.term
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .set_options(options);
        true
    }

    /// Resize to exact geometry, preserving retry state when the native PTY
    /// rejects the request.
    pub fn try_resize_geometry(&mut self, desired: PtyGeometry) -> Result<()> {
        let current = self
            .geometry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .geometry;
        let grid_changed = (current.columns, current.rows) != (desired.columns, desired.rows);
        let columns_reflowed = current.columns != desired.columns;
        let local_changed = current != desired;
        let resize_native = native_resize_required(self.applied_pty_geometry, desired);
        if !local_changed && !resize_native {
            return Ok(());
        }

        let native_result = if resize_native {
            self.master
                .as_ref()
                .map_or(Ok(()), |master| master.resize(native_pty_size(desired)))
                .context("native pseudoterminal resize")
        } else {
            Ok(())
        };
        if native_result.is_ok() {
            // On Windows a pixel-only update deliberately skips
            // ResizePseudoConsole; recording the desired pixels here is safe
            // because future native comparisons ignore them on that platform.
            self.applied_pty_geometry = desired;
        }

        // Graphics chunks take this gate before observing Term -> geometry.
        // Taking it before the local grid commit makes reflow invalidation and
        // placement insertion a single ordered operation.
        let _graphics_guard = self
            .graphics_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let grid_options = if grid_changed {
            // The cap only ever RISES for the life of a pane.
            //
            // `effective_scrollback_lines` turns the byte budget into a line
            // count by dividing it by a worst-case per-row cost at the current
            // column count, so a wider pane yields a smaller cap. Assigning
            // that unconditionally made every widen hand `Grid::update_history`
            // a lower limit, and it trims from the oldest end — immediately and
            // irreversibly. Four ordinary gestures reach it: dragging the
            // window wider (each intermediate width applying its own cap),
            // decrease-font, closing a sibling split, and un-zooming. Measured
            // with the shipped defaults: 77 columns held 5202 lines, 241 held
            // 1681, and dragging back to 77 did not restore one of them.
            //
            // Nothing about a resize means the user wants less history, so the
            // budget must not be enforced by a resize. The ceiling still falls
            // out of the same computation — it just cannot move down, which
            // makes the worst case the budget measured at the width the
            // history was accumulated at, bounded and paid only by a user who
            // actually widened.
            self.term_config.scrolling_history = scrollback_cap_after_resize(
                self.term_config.scrolling_history,
                self.scrollback_line_limit,
                self.scrollback_byte_limit,
                desired.columns,
                desired.rows,
            );
            Some(self.term_config.clone())
        } else {
            None
        };
        commit_local_geometry(&self.term, &self.geometry, desired, grid_options);
        if grid_changed {
            let oldest_abs = self
                .term
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .grid()
                .history_origin();
            prune(&self.images, oldest_abs);
        }
        if columns_reflowed {
            clear_reflowed_regular_placements(
                &self.images,
                &self.relatives,
                &self.inactive_graphics,
            );
            self.graphics_reflow_generation
                .fetch_add(1, std::sync::atomic::Ordering::Release);
        } else if local_changed {
            if let Ok(mut placements) = self.images.lock() {
                recompute_kitty_placements(&mut placements, desired);
            }
            let mut inactive = self
                .inactive_graphics
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            recompute_kitty_placements(&mut inactive.placements, desired);
        }
        if columns_reflowed && let Ok(mut prompts) = self.prompts.lock() {
            // Reflow can merge or split logical rows. Their previous row ids
            // are intentionally not guessed across that shape change.
            prompts.clear();
        }

        self.cols = desired.columns;
        self.rows = desired.rows;
        native_result
    }

    /// Exact text-area pixel dimensions last published to the PTY.
    ///
    /// This is the authoritative CSI 14t response as well as the ConPTY/tmux
    /// winsize. It may not be evenly divisible by the grid at fractional DPI.
    pub fn pty_pixel_size(&self) -> (u16, u16) {
        (
            self.applied_pty_geometry.pixel_width,
            self.applied_pty_geometry.pixel_height,
        )
    }

    /// Quiesce or re-enable output-driven event-loop wakes for this pane.
    pub fn set_output_wake_enabled(&self, enabled: bool) {
        self.output_wake.set_enabled(enabled);
    }

    /// Re-open this pane's output latch before the UI reads its generation.
    pub fn acknowledge_output_wake(&self) {
        self.output_wake.acknowledge();
    }

    /// Whether this pane's bounded semantic-event queue overflowed. Overflow is
    /// sticky and is an explicit fail-pane condition; callers must not continue
    /// as though reply-bearing events were delivered.
    pub fn event_queue_overflowed(&self) -> bool {
        self.event_overflowed.load(Ordering::Acquire)
    }

    /// Has the child process exited?
    pub fn child_exited(&self) -> bool {
        self.child
            .lock()
            .ok()
            .and_then(|mut c| c.try_wait().ok().flatten())
            .is_some()
    }

    /// Agent-first (A1): kill the child immediately.
    /// Used by `kettle exec --timeout` when the deadline fires. The reader
    /// thread sees EOF when the master closes on drop, so no extra teardown is
    /// needed here.
    ///
    /// The outcome is returned rather than swallowed. A child that had already
    /// exited counts as success — that is what the caller wanted — but a
    /// genuine failure to terminate means the process is still running, and a
    /// caller about to report a timeout should be able to say so. This was
    /// unusable before the Windows path was corrected: it reported every
    /// successful kill as an error and every real failure as success.
    pub fn kill(&self) -> std::io::Result<()> {
        let outcome = match self.child.lock() {
            Ok(mut c) => c.kill(),
            Err(_) => {
                return Err(std::io::Error::other(
                    "child handle is poisoned; cannot terminate",
                ));
            }
        };
        match outcome {
            // "It already exited" is the outcome the caller asked for, and it
            // is the COMMON case on the timeout path: the deadline and the
            // child finishing race every time. Unix reports that race as
            // `ESRCH` from `kill(2)`; Windows reports it as access-denied on a
            // dead handle and is normalized a layer below. Treating either as
            // a failure would make `kettle exec` announce that a process may
            // still be running precisely when it definitely is not.
            Err(error) if error.raw_os_error() == Some(libc_esrch()) => Ok(()),
            other => other,
        }
    }

    /// Agent-first (A1): the child's exit status, if it has exited
    /// (non-blocking `try_wait`). `child_exited` discards the status; the
    /// headless `kettle exec` path needs it to propagate the child's exit code
    /// to its own process exit. `None` while the child is still running (or if
    /// the child handle is poisoned).
    ///
    /// The vendored portable-pty decodes Unix signal death into the shell's
    /// `128 + signo`, so SIGTERM is `143` and SIGKILL is `137`. It previously
    /// collapsed every signal death to a generic `1`, which made a killed
    /// command indistinguishable from one that merely failed — a distinction
    /// agent automation driving `kettle exec` depends on. The numeric signal is
    /// retained alongside its name if a caller needs to act on it directly.
    /// Callers clamp to 0..=255 on Unix before `std::process::exit` regardless.
    pub fn child_exit_code(&self) -> Option<u32> {
        self.child
            .lock()
            .ok()
            .and_then(|mut c| c.try_wait().ok().flatten())
            .map(|st| st.exit_code())
    }

    /// Agent-first (A1/A2): a plain-text snapshot of the grid.
    /// Without extra scrollback, this is the visible viewport; with
    /// `scrollback_lines`, it returns that many history rows (newest history
    /// first in document order) followed by the active screen for command-output
    /// capture. One lock acquisition; per-line trailing whitespace trimmed;
    /// `scrollback_lines` hard-capped at 10_000 so a hostile/buggy caller can't
    /// ask for an unbounded join. Shared by the control server's `read_screen`,
    /// `run_command` output slicing, and any future scripting surface — the
    /// single sanctioned grid-scrape.
    pub fn screen_text(&self, scrollback_lines: usize) -> Option<ScreenText> {
        let t = self.term.lock().ok()?;
        Some(screen_text_of(&t, scrollback_lines))
    }
}

/// Pure body of [`Terminal::screen_text`], factored on a raw `Term` so the
/// no-PTY conformance harness can exercise it without spawning a child.
pub fn screen_text_of(t: &Term<EventProxy>, scrollback_lines: usize) -> ScreenText {
    const MAX_SCROLLBACK_LINES: usize = 10_000;
    let grid = t.grid();
    let cols = grid.columns();
    let rows = grid.screen_lines();
    let history_size = grid.history_size();
    let display_offset = grid.display_offset();
    let take = scrollback_lines.min(history_size).min(MAX_SCROLLBACK_LINES);
    let mut text = String::with_capacity((take + rows) * (cols + 1));
    let mut line = String::with_capacity(cols);
    let display_adjust = if take == 0 { display_offset as i32 } else { 0 };
    for r in -(take as i32)..rows as i32 {
        line.clear();
        // Spacer-aware so the agent screen-scrape doesn't inject a space after
        // every wide (CJK/emoji) glyph (v2.26.0, shared helper).
        crate::grid_text::append_row_text(grid, r - display_adjust, cols, &mut line);
        text.push_str(line.trim_end());
        text.push('\n');
    }
    let cur = grid.cursor.point;
    ScreenText {
        text,
        cols,
        rows,
        history_size,
        display_offset,
        cursor: (cur.line.0.max(0) as usize, cur.column.0),
        // v2.20.0 (agent plane): DEC ?25 visibility — vim/fzf/less hide the
        // cursor; an agent placing keystrokes by cursor position needs to
        // know when the reported point is meaningless.
        cursor_visible: t
            .mode()
            .contains(alacritty_terminal::term::TermMode::SHOW_CURSOR),
    }
}

/// Agent-first: the result of [`Terminal::screen_text`] — the
/// joined plain text plus the grid geometry an agent needs to interpret it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenText {
    /// History tail + active screen, newline-joined, per-line right-trimmed.
    pub text: String,
    pub cols: usize,
    pub rows: usize,
    /// Total scrollback lines available (not how many were returned).
    pub history_size: usize,
    /// Current scroll position (0 = at the bottom).
    pub display_offset: usize,
    /// Cursor (row in the active screen, col).
    pub cursor: (usize, usize),
    /// v2.20.0: whether the cursor is shown (DEC ?25; vim/fzf/less hide it).
    pub cursor_visible: bool,
}

/// Close the PTY while its output reader is still allowed to drain.
///
/// Before Windows 11 24H2, `ClosePseudoConsole` can wait for clients to finish
/// writing their final output. Stopping Kettle's parser/pump first can fill the
/// bounded handoff and strand that close forever. The stop publication must
/// therefore happen only after the platform close has returned. Unix uses the
/// same ordering so teardown has one portable lifetime contract.
fn close_pty_while_reader_is_live(stop: &AtomicBool, close_pty: impl FnOnce()) {
    close_pty();
    stop.store(true, Ordering::Relaxed);
}

impl Drop for Terminal {
    /// Tear down the PTY WITHOUT ever blocking the calling thread.
    ///
    /// This runs on the UI thread — closing a pane drops the owned
    /// `Pane.term` (`Mux::close_focused` → `panes.remove`) — so blocking here
    /// freezes the whole window.
    ///
    /// The previous body `join()`ed the reader thread while the master PTY was
    /// still alive. The reader sits in a blocking `read()` on the ConPTY
    /// conout pipe that only returns once the pseudoconsole is *closed* — but
    /// the master (hence `ClosePseudoConsole`) wasn't dropped until after this
    /// function returned, so the join could never complete and the UI thread
    /// deadlocked. Windows then showed the window as "not responding", which
    /// users reported as a crash. (Reproduced on build 26200: close-split left
    /// the process alive with `Responding=false` for as long as it was sampled
    /// — a hang, not a panic. See `target/pty-drop-repro.txt`.)
    ///
    /// Drop releases the input writer, then a detached reaper kills the child
    /// and closes the master (conout / pseudoconsole) while the output reader
    /// remains live. Teardown never depends on the writer synthesizing EOF.
    /// Only after the master close returns does the reaper publish the reader
    /// stop flag and reap the child. This ordering follows the Win32
    /// contract for pre-24H2 `ClosePseudoConsole`, which may wait indefinitely
    /// if conout is neither closed nor drained. `Drop` itself only moves owned
    /// handles to the reaper and detaches the parser handle, so the UI keeps
    /// pumping. The reader owns only `Arc` clones (no borrow of `Terminal`), so
    /// it is sound for it to outlive this `Drop`. Unix uses the same ordering
    /// (master fd close → reader stop) without a platform-specific branch.
    fn drop(&mut self) {
        // 1. Let the pump bypass a full parser queue and drain/discard conout
        //    directly for the remainder of teardown.
        self.drain_output.store(true, Ordering::Release);
        // 2. Stop accepting input by swapping in a discard sink and dropping
        //    the writer handle. Unix closes only the duplicate descriptor and
        //    deliberately sends no terminal input; the reaper below owns the
        //    explicit child shutdown.
        if let Ok(mut w) = self.writer.try_lock() {
            let _ = std::mem::replace(&mut *w, Box::new(NullWrite));
        }
        // 3. Kill/reap the child and close the master on a detached thread.
        //    `ClosePseudoConsole` itself can wait for the conout drain on
        //    Windows, so merely avoiding a reader `join()` is insufficient:
        //    dropping the master on the UI thread can still freeze pane close,
        //    and stopping the output reader first can deadlock the detached
        //    close on Windows versions before 11 24H2.
        //    The indirection keeps the master alive if thread creation fails;
        //    leaking one failed teardown is preferable to synchronously
        //    entering a platform close that has no deadline.
        #[cfg(windows)]
        if let Some(close) = &self.pty_close {
            close.terminal_dropped(&self.stop);
        }
        #[cfg(windows)]
        let close_owns_reader_stop = self.pty_close.is_some();
        #[cfg(not(windows))]
        let close_owns_reader_stop = false;
        let teardown = Arc::new(Mutex::new(Some((self.child.clone(), self.master.take()))));
        let teardown_worker = Arc::clone(&teardown);
        let reaper_stop = Arc::clone(&self.stop);
        if let Err(error) = std::thread::Builder::new()
            .name("kettle-pty-reaper".into())
            .spawn(move || {
                let teardown = teardown_worker
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .take();
                let Some((child, master)) = teardown else {
                    return;
                };
                if let Ok(mut child) = child.lock() {
                    let _ = child.kill();
                }
                if close_owns_reader_stop {
                    debug_assert!(master.is_none());
                    // `begin_pty_output_close` moved the master to another
                    // worker. That worker alone publishes `stop` after the
                    // real platform close returns; doing it here first can
                    // deadlock ClosePseudoConsole against its own reader.
                    drop(master);
                } else {
                    close_pty_while_reader_is_live(&reaper_stop, || {
                        drop(master);
                    });
                }
                if let Ok(mut child) = child.lock() {
                    let _ = child.wait();
                }
            })
        {
            log::error!("failed to spawn PTY teardown worker: {error}");
            self.stop.store(true, Ordering::Relaxed);
            std::mem::forget(teardown);
        }
        // 4. DETACH the reader thread — never join() on the UI thread.
        drop(self.reader_thread.take());
    }
}

impl EventProxy {
    fn send_event_exit(&self) {
        use alacritty_terminal::event::EventListener;
        self.send_event(TermEvent::Exit);
    }
}

/// Agent-first (A1): a cloneable, thread-safe handle to a PTY's write
/// side, obtained from [`Terminal::writer_handle`]. Lets a worker thread feed
/// input to the child without sharing the (non-`Sync`) `Terminal`.
#[derive(Clone)]
pub struct PtyWriter(Arc<Mutex<Box<dyn Write + Send>>>);

/// Write one complete message through a possibly nonblocking PTY writer.
///
/// `PIPE_NOWAIT` historically surfaced a full byte pipe as `Ok(0)`, while
/// conforming wrappers return `WouldBlock`. Treat both as transient
/// backpressure, preserve partial progress, and bound consecutive stalls. The
/// callback is separated from the state machine so portable unit tests can
/// exercise every branch without sleeping.
fn write_all_with_backpressure<W, F>(
    writer: &mut W,
    bytes: &[u8],
    chunk_limit: usize,
    max_backpressure_retries: usize,
    mut wait: F,
) -> io::Result<()>
where
    W: Write + ?Sized,
    F: FnMut(),
{
    let chunk_limit = chunk_limit.max(1);
    let mut offset = 0usize;
    let mut consecutive_backpressure = 0usize;

    while offset < bytes.len() {
        let end = offset.saturating_add(chunk_limit).min(bytes.len());
        match writer.write(&bytes[offset..end]) {
            Ok(0) => {
                consecutive_backpressure = consecutive_backpressure.saturating_add(1);
            }
            Ok(written) if written <= end - offset => {
                offset += written;
                consecutive_backpressure = 0;
                continue;
            }
            Ok(written) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "PTY writer reported {written} bytes for a {}-byte request",
                        end - offset
                    ),
                ));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                consecutive_backpressure = consecutive_backpressure.saturating_add(1);
            }
            Err(error) => return Err(error),
        }

        if consecutive_backpressure > max_backpressure_retries {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!(
                    "PTY input remained full after {max_backpressure_retries} retries \
                     ({offset} of {} bytes delivered)",
                    bytes.len()
                ),
            ));
        }
        wait();
    }

    let mut flush_backpressure = 0usize;
    loop {
        match writer.flush() {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                flush_backpressure = flush_backpressure.saturating_add(1);
                if flush_backpressure > max_backpressure_retries {
                    return Err(io::Error::new(
                        io::ErrorKind::WouldBlock,
                        format!(
                            "PTY input flush remained blocked after \
                             {max_backpressure_retries} retries"
                        ),
                    ));
                }
                wait();
            }
            Err(error) => return Err(error),
        }
    }
}

fn complete_write_chunk_limit(message_len: usize) -> usize {
    #[cfg(windows)]
    {
        message_len.min(CONPTY_NONBLOCKING_WRITE_BYTES)
    }
    #[cfg(not(windows))]
    {
        message_len
    }
}

impl PtyWriter {
    /// Write and flush a complete queued input/reply message.
    ///
    /// Partial writes are retained and a temporarily full nonblocking PTY is
    /// retried in bounded increments. If backpressure persists for roughly two
    /// seconds, the error remains classified as `WouldBlock` and includes the
    /// delivered byte count; it is never rewritten as a fatal `WriteZero`.
    /// Latency-sensitive owners should use [`PtyStdin::try_write`] and retain
    /// the pending suffix in their own bounded queue.
    pub fn write_all_checked(&self, bytes: &[u8]) -> Result<()> {
        let mut writer = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY writer lock poisoned"))?;
        write_all_with_backpressure(
            &mut **writer,
            bytes,
            complete_write_chunk_limit(bytes.len()),
            PTY_COMPLETE_WRITE_MAX_BACKPRESSURE_RETRIES,
            || std::thread::sleep(std::time::Duration::from_millis(1)),
        )
        .context("cannot write queued input to child PTY")
    }

    fn write_some_checked(&self, bytes: &[u8]) -> Result<usize> {
        let mut writer = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("PTY writer lock poisoned while forwarding stdin"))?;
        match writer.write(bytes) {
            Ok(written) => Ok(written),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(0),
            Err(error) => Err(error).context("cannot write forwarded stdin to child PTY"),
        }
    }

    /// Write all of `bytes` to the PTY using the bounded complete-message
    /// contract. Failures are logged so a suffix is never silently discarded.
    pub fn write(&self, bytes: &[u8]) {
        if let Err(error) = self.write_all_checked(bytes) {
            log::error!("cannot deliver complete queued input to child PTY: {error:#}");
        }
    }

    /// Drop Kettle's PTY input writer and replace it with a discard sink.
    ///
    /// A PTY does not provide a portable stdin half-close: closing ConPTY input
    /// can terminate the attached process, while a Unix PTY master must remain
    /// open for terminal-query replies. Exec-style forwarding should use
    /// [`PtyStdin::try_signal_eof`] and handle
    /// [`PtyEofProgress::Unsupported`] explicitly.
    #[deprecated(note = "use PtyStdin::try_signal_eof; PTYs have no portable input half-close")]
    pub fn close(&self) {
        if let Ok(mut w) = self.0.lock() {
            let _ = std::mem::replace(&mut *w, Box::new(NullWrite));
        }
    }
}

#[cfg(test)]
mod complete_pty_write_tests {
    use super::write_all_with_backpressure;
    use std::collections::VecDeque;
    use std::io::{self, Write};

    enum Step {
        Accept(usize),
        Zero,
        WouldBlock,
        Interrupted,
    }

    struct ScriptedWriter {
        steps: VecDeque<Step>,
        written: Vec<u8>,
        flush_would_block: usize,
    }

    impl ScriptedWriter {
        fn new(steps: impl IntoIterator<Item = Step>) -> Self {
            Self {
                steps: steps.into_iter().collect(),
                written: Vec::new(),
                flush_would_block: 0,
            }
        }
    }

    impl Write for ScriptedWriter {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            match self.steps.pop_front().unwrap_or(Step::Accept(bytes.len())) {
                Step::Accept(limit) => {
                    let accepted = limit.min(bytes.len());
                    self.written.extend_from_slice(&bytes[..accepted]);
                    Ok(accepted)
                }
                Step::Zero => Ok(0),
                Step::WouldBlock => Err(io::Error::from(io::ErrorKind::WouldBlock)),
                Step::Interrupted => Err(io::Error::from(io::ErrorKind::Interrupted)),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.flush_would_block == 0 {
                Ok(())
            } else {
                self.flush_would_block -= 1;
                Err(io::Error::from(io::ErrorKind::WouldBlock))
            }
        }
    }

    #[test]
    fn retries_zero_would_block_interruption_and_partial_progress_without_loss() {
        let mut writer = ScriptedWriter::new([
            Step::Zero,
            Step::WouldBlock,
            Step::Interrupted,
            Step::Accept(2),
            Step::WouldBlock,
            Step::Accept(usize::MAX),
        ]);
        writer.flush_would_block = 1;
        let mut waits = 0;

        write_all_with_backpressure(&mut writer, b"abcdef", 4, 3, || waits += 1)
            .expect("transient backpressure must recover");

        assert_eq!(writer.written, b"abcdef");
        assert_eq!(
            waits, 4,
            "zero, two WouldBlock writes, and one blocked flush wait"
        );
    }

    #[test]
    fn persistent_zero_progress_is_would_block_with_an_actionable_prefix_count() {
        let mut writer = ScriptedWriter::new([Step::Accept(2), Step::Zero, Step::Zero, Step::Zero]);
        let error = write_all_with_backpressure(&mut writer, b"abcd", 4, 2, || {})
            .expect_err("bounded retry exhaustion must be reported");

        assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
        assert!(
            error.to_string().contains("2 of 4 bytes delivered"),
            "partial progress must be explicit: {error}"
        );
        assert_eq!(writer.written, b"ab");
        assert_ne!(error.kind(), io::ErrorKind::WriteZero);
    }

    #[test]
    fn successful_progress_resets_the_consecutive_backpressure_bound() {
        let mut writer = ScriptedWriter::new([
            Step::Zero,
            Step::Zero,
            Step::Accept(1),
            Step::WouldBlock,
            Step::WouldBlock,
            Step::Accept(1),
        ]);

        write_all_with_backpressure(&mut writer, b"ab", 1, 2, || {})
            .expect("progress between stalls resets the bound");
        assert_eq!(writer.written, b"ab");
    }
}

/// Shared ownership state for Unix's PTY-master `O_NONBLOCK` lease.
///
/// This state machine is portable so its exclusivity and failed-restoration
/// semantics can be tested on every host. Only Unix stores it in `Terminal`.
#[cfg(any(unix, test))]
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
enum PtyStdinLeasePhase {
    #[default]
    Available,
    Active,
    RestoreFailed,
}

#[cfg(any(unix, test))]
impl PtyStdinLeasePhase {
    fn try_begin(&mut self) -> std::result::Result<(), &'static str> {
        match self {
            Self::Available => {
                *self = Self::Active;
                Ok(())
            }
            Self::Active => Err("a PTY stdin nonblocking lease is already active"),
            Self::RestoreFailed => {
                Err("the previous PTY stdin lease could not restore file status flags")
            }
        }
    }

    fn abort_begin(&mut self) {
        debug_assert_eq!(*self, Self::Active);
        *self = Self::Available;
    }

    fn finish(&mut self, restored: bool) {
        debug_assert_eq!(*self, Self::Active);
        *self = if restored {
            Self::Available
        } else {
            Self::RestoreFailed
        };
    }
}

#[cfg(test)]
mod pty_stdin_lease_phase_tests {
    use super::PtyStdinLeasePhase;

    #[cfg(unix)]
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
    #[cfg(unix)]
    use std::sync::{Arc, Mutex};

    #[cfg(unix)]
    use super::{UnixPtyStdinLease, fcntl_retry};

    #[test]
    fn lease_is_exclusive_until_restoration_finishes() {
        let mut phase = PtyStdinLeasePhase::Available;

        phase.try_begin().expect("first lease is available");
        assert_eq!(
            phase.try_begin(),
            Err("a PTY stdin nonblocking lease is already active")
        );

        phase.finish(true);
        phase.try_begin().expect("restored lease is reusable");
    }

    #[test]
    fn failed_setup_releases_the_reserved_lease() {
        let mut phase = PtyStdinLeasePhase::Available;

        phase.try_begin().expect("lease reservation succeeds");
        phase.abort_begin();

        phase
            .try_begin()
            .expect("a setup failure must not permanently consume the lease");
    }

    #[test]
    fn failed_restoration_latches_the_lease_closed() {
        let mut phase = PtyStdinLeasePhase::Available;

        phase.try_begin().expect("lease reservation succeeds");
        phase.finish(false);

        assert_eq!(
            phase.try_begin(),
            Err("the previous PTY stdin lease could not restore file status flags")
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_lease_sets_exclusive_nonblocking_status_and_restores_it() {
        let mut descriptors = [-1; 2];
        assert_eq!(
            unsafe { libc::pipe(descriptors.as_mut_ptr()) },
            0,
            "pipe fixture creation failed: {}",
            std::io::Error::last_os_error()
        );
        let _read_end = unsafe { OwnedFd::from_raw_fd(descriptors[0]) };
        let write_end = unsafe { OwnedFd::from_raw_fd(descriptors[1]) };
        let phase = Arc::new(Mutex::new(PtyStdinLeasePhase::Available));
        let original = fcntl_retry(write_end.as_raw_fd(), libc::F_GETFL, 0)
            .expect("read original pipe status");

        let lease = UnixPtyStdinLease::acquire(write_end.as_raw_fd(), Arc::clone(&phase))
            .expect("first lease succeeds");
        let active =
            fcntl_retry(write_end.as_raw_fd(), libc::F_GETFL, 0).expect("read active pipe status");
        assert_ne!(active & libc::O_NONBLOCK, 0);
        assert!(
            UnixPtyStdinLease::acquire(write_end.as_raw_fd(), Arc::clone(&phase)).is_err(),
            "a second live lease must be rejected"
        );

        drop(lease);
        assert_eq!(
            fcntl_retry(write_end.as_raw_fd(), libc::F_GETFL, 0)
                .expect("read restored pipe status"),
            original
        );

        drop(
            UnixPtyStdinLease::acquire(write_end.as_raw_fd(), phase)
                .expect("the restored lease is reusable"),
        );
    }
}

#[cfg(unix)]
fn fcntl_retry(
    fd: RawFd,
    command: libc::c_int,
    argument: libc::c_int,
) -> std::io::Result<libc::c_int> {
    loop {
        let result = unsafe { libc::fcntl(fd, command, argument) };
        if result >= 0 {
            return Ok(result);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// The unique live owner of Unix PTY-master nonblocking status.
///
/// Every master/writer/reader descriptor is a `dup` of the same open-file
/// description, so `F_SETFL` through any one of them changes all of them.
/// Exclusivity makes the captured flags and their one Drop-time restoration a
/// properly nested pair instead of independently scoped, overlapping changes.
#[cfg(unix)]
struct UnixPtyStdinLease {
    termios_fd: OwnedFd,
    original_status_flags: libc::c_int,
    phase: Arc<Mutex<PtyStdinLeasePhase>>,
}

#[cfg(unix)]
impl UnixPtyStdinLease {
    fn acquire(master_fd: RawFd, phase: Arc<Mutex<PtyStdinLeasePhase>>) -> Result<Self> {
        let mut current = phase
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        current.try_begin().map_err(anyhow::Error::msg)?;

        let acquired = (|| {
            let duplicated = fcntl_retry(master_fd, libc::F_DUPFD_CLOEXEC, 0)
                .context("cannot duplicate PTY descriptor for stdin termios")?;
            let termios_fd = unsafe { OwnedFd::from_raw_fd(duplicated) };
            let original_status_flags = fcntl_retry(termios_fd.as_raw_fd(), libc::F_GETFL, 0)
                .context("cannot read PTY status flags for stdin arbitration")?;
            fcntl_retry(
                termios_fd.as_raw_fd(),
                libc::F_SETFL,
                original_status_flags | libc::O_NONBLOCK,
            )
            .context("cannot make PTY input nonblocking for stdin arbitration")?;
            Ok::<_, anyhow::Error>((termios_fd, original_status_flags))
        })();

        let (termios_fd, original_status_flags) = match acquired {
            Ok(acquired) => acquired,
            Err(error) => {
                current.abort_begin();
                return Err(error);
            }
        };
        drop(current);
        Ok(Self {
            termios_fd,
            original_status_flags,
            phase,
        })
    }

    fn fd(&self) -> RawFd {
        self.termios_fd.as_raw_fd()
    }
}

#[cfg(unix)]
impl Drop for UnixPtyStdinLease {
    fn drop(&mut self) {
        let restoration = {
            let mut current = self
                .phase
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let restoration = fcntl_retry(
                self.termios_fd.as_raw_fd(),
                libc::F_SETFL,
                self.original_status_flags,
            );
            current.finish(restoration.is_ok());
            restoration
        };
        if let Err(error) = restoration {
            // Keep the shared phase failed closed: a later handle must not
            // capture the still-nonblocking status as its "original" flags.
            log::error!("cannot restore PTY status flags after stdin arbitration: {error}");
        }
    }
}

/// Writer-arbiter-owned PTY input handle.
///
/// Keeping every forwarded byte and terminal reply on one worker prevents
/// canonical-mode backpressure from stalling the exec owner loop's timeout,
/// cancellation, query, and child lifecycle handling. Unix uses nonblocking
/// writes plus checked termios snapshots; Windows preserves ConPTY input after
/// pipe EOF because closing it terminates the child instead of half-closing it.
pub struct PtyStdin {
    writer: PtyWriter,
    #[cfg(unix)]
    lease: UnixPtyStdinLease,
    #[cfg(unix)]
    input_state: PtyInputTail,
    #[cfg(unix)]
    pending_eof: Option<PendingPtyEof>,
}

/// Result of one nonblocking EOF-injection step.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PtyEofProgress {
    /// PTY capacity is exhausted or another VEOF byte remains. The caller
    /// should service higher-priority replies before retrying.
    Pending,
    /// The configured canonical VEOF sequence was delivered.
    Signaled,
    /// The platform or current terminal mode has no safe EOF signal, including
    /// Unix noncanonical or `EXTPROC` modes and Windows ConPTY.
    Unsupported,
}

#[cfg(unix)]
struct PendingPtyEof {
    sequence: [u8; 2],
    sequence_len: usize,
    offset: usize,
    rules: CanonicalEofRules,
    configured_veof: u8,
    disabled: u8,
}

impl PtyStdin {
    /// Try one exact write without waiting for Unix PTY capacity. `Ok(0)`
    /// means the nonblocking Unix descriptor would block.
    pub fn try_write(&mut self, bytes: &[u8]) -> Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        #[cfg(unix)]
        {
            let fd = self.lease.fd();
            let (before, _, _) = live_canonical_eof_rules(fd).inspect_err(|_| {
                self.input_state.ambiguous_termios = true;
            })?;
            let written = self.writer.write_some_checked(bytes).inspect_err(|_| {
                self.input_state.ambiguous_termios = true;
            })?;
            self.input_state.observe(&bytes[..written], before);
            let (after, _, _) = live_canonical_eof_rules(fd).inspect_err(|_| {
                self.input_state.ambiguous_termios = true;
            })?;
            if before != after {
                self.input_state.ambiguous_termios = true;
            }
            Ok(written)
        }
        #[cfg(not(unix))]
        {
            // PIPE_NOWAIT may return zero rather than a partial write when one
            // request exceeds the anonymous pipe's currently available quota.
            // Keep requests below the default ConPTY pipe quantum so forward
            // progress does not require an entirely empty 8 KiB queue.
            self.writer
                .write_some_checked(&bytes[..bytes.len().min(CONPTY_NONBLOCKING_WRITE_BYTES)])
        }
    }

    /// Advance canonical EOF injection by at most one VEOF byte.
    ///
    /// This is deliberately incremental: a full PTY input buffer must not
    /// trap the writer worker in an internal retry loop while a later terminal
    /// query reply waits. Callers retry [`PtyEofProgress::Pending`] only after
    /// servicing their higher-priority reply queue.
    pub fn try_signal_eof(&mut self) -> Result<PtyEofProgress> {
        #[cfg(unix)]
        {
            if self.pending_eof.is_none() {
                let (rules, configured_veof, disabled) = live_canonical_eof_rules(self.lease.fd())?;
                if !rules.supports_eof() {
                    return Ok(PtyEofProgress::Unsupported);
                }
                let unterminated_record = self
                    .input_state
                    .record_unterminated(rules)
                    .map_err(anyhow::Error::msg)?;
                let Some((sequence, sequence_len)) = pty_eof_sequence(
                    rules.canonical,
                    configured_veof,
                    disabled,
                    unterminated_record,
                )
                .map_err(anyhow::Error::msg)?
                else {
                    return Ok(PtyEofProgress::Unsupported);
                };
                self.pending_eof = Some(PendingPtyEof {
                    sequence,
                    sequence_len,
                    offset: 0,
                    rules,
                    configured_veof,
                    disabled,
                });
            } else {
                let (rules, configured_veof, disabled) = live_canonical_eof_rules(self.lease.fd())?;
                let pending = self.pending_eof.as_ref().expect("checked above");
                if rules != pending.rules
                    || configured_veof != pending.configured_veof
                    || disabled != pending.disabled
                {
                    self.pending_eof = None;
                    self.input_state.ambiguous_termios = true;
                    anyhow::bail!("the child changed termios during canonical EOF injection");
                }
            }

            let pending = self.pending_eof.as_mut().expect("initialized above");
            let end = pending.offset + 1;
            let written = self
                .writer
                .write_some_checked(&pending.sequence[pending.offset..end])?;
            if written == 0 {
                return Ok(PtyEofProgress::Pending);
            }
            pending.offset += written;
            if pending.offset < pending.sequence_len {
                Ok(PtyEofProgress::Pending)
            } else {
                self.pending_eof = None;
                Ok(PtyEofProgress::Signaled)
            }
        }
        #[cfg(not(unix))]
        {
            // Closing ConPTY input terminates the attached process with
            // STATUS_CONTROL_C_EXIT instead of delivering a Unix-like
            // half-close. Preserve the child and its terminal-query channel;
            // line-/protocol-delimited Windows consumers still receive every
            // forwarded byte and EOF remains an explicit unsupported state.
            Ok(PtyEofProgress::Unsupported)
        }
    }
}

/// Parser state for [`AnsiStripper`], carried across calls so a CSI/OSC
/// sequence split across two PTY-read chunks (each capped at
/// `PTY_READ_BUFFER_BYTES`) is still recognized as a single sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum StripState {
    /// Not inside any escape sequence; plain bytes pass through.
    #[default]
    Plain,
    /// Just saw a bare ESC; the next byte decides CSI (`[`), OSC (`]`), or a
    /// single-char escape (anything else).
    EscSeen,
    /// Inside an ESC sequence whose intermediates are `0x20..=0x2f`, waiting
    /// for its `0x30..=0x7e` final byte.
    EscapeIntermediate,
    /// Inside `ESC [ params...`, scanning for the CSI final byte
    /// (`0x40..=0x7e`).
    Csi,
    /// Inside `ESC ] ...`, scanning for BEL (`0x07`) or ST.
    Osc,
    /// Inside a DCS/APC/PM/SOS control string (`ESC P`, `ESC _`, `ESC ^`,
    /// `ESC X`), scanning for ST.
    ///
    /// These were previously treated as single-character escapes, so only the
    /// two introducer bytes were removed and the entire BODY was written to
    /// the session log as text — Sixel pixel data and Kitty graphics payloads,
    /// which carry encoded file paths and shared-memory names. A log the user
    /// enabled to keep a transcript was instead accumulating binary payloads.
    String,
}

/// Terminator parity (`plugins/logger.py` extension): a persistent-state
/// stripper for ANSI/OSC escape sequences, used by the per-pane session-log
/// path (`log_strip_ansi`). Recognizes:
///   - CSI (Control Sequence Introducer): `ESC [ params final`
///     where final is in `0x40..=0x7e`.
///   - OSC (Operating System Command): `ESC ] ... terminator`
///     where terminator is BEL (0x07) or ST (`ESC \\`).
///   - Single-char ESC: `ESC X` for any other X.
///
/// Unlike a stateless scan, `AnsiStripper` carries its FSM state across
/// `strip` calls: a CSI/OSC sequence whose terminator lands in the *next*
/// 64 KiB PTY read is still tracked as "mid-sequence" and its continuation
/// bytes (raw SGR parameters, the tail of an OSC 8 URI, a window title) are
/// dropped rather than leaking into the log as literal text. Plain
/// printable bytes + newlines pass through unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AnsiStripper {
    state: StripState,
    /// UTF-8 continuation bytes still owed by the character being decoded.
    ///
    /// `0x9c` is both the 8-bit ST and a UTF-8 continuation byte, so a payload
    /// containing `末` (`e6 9c ab`) ended the control string at the middle of
    /// that character and leaked the remainder into the log. The VT extractor
    /// already draws this distinction; the log stripper has to as well.
    utf8_continuation: u8,
    /// Whether the lead byte of the current UTF-8 scalar reached the log.
    /// Continuations follow their lead across parser-state transitions.
    utf8_emitted: bool,
}

impl AnsiStripper {
    /// A fresh stripper, starting outside any escape sequence.
    pub fn new() -> Self {
        Self::default()
    }

    /// Strip ANSI escape sequences from `input`, resuming any sequence left
    /// in progress by a previous call on `self`. See the type docs for the
    /// sequence forms recognized.
    pub fn strip(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());
        for &b in input {
            // Mid-character: this byte belongs to the character being decoded
            // and cannot be a control, in any state. A byte that is not a
            // continuation means the lead was malformed, so stop shielding at
            // once rather than swallowing what follows.
            if self.utf8_continuation > 0 {
                if matches!(b, 0x80..=0xbf) {
                    self.utf8_continuation -= 1;
                    if self.utf8_emitted {
                        out.push(b);
                    }
                    continue;
                }
                self.utf8_continuation = 0;
            }
            // Track leads in every state. A malformed escape can contain one,
            // and consuming its lead while emitting continuations after the
            // state returns to Plain would manufacture invalid UTF-8.
            self.utf8_continuation = match b {
                0xc2..=0xdf => 1,
                0xe0..=0xef => 2,
                0xf0..=0xf4 => 3,
                _ => 0,
            };
            self.utf8_emitted = matches!(self.state, StripState::Plain);
            match self.state {
                StripState::Plain => {
                    if b == 0x1b {
                        self.state = StripState::EscSeen;
                    } else {
                        out.push(b);
                    }
                }
                StripState::EscSeen => {
                    self.state = Self::escape_follower(b);
                }
                StripState::EscapeIntermediate => {
                    if b == 0x1b {
                        self.state = StripState::EscSeen;
                    } else if b == 0x18 || b == 0x1a || (0x30..=0x7e).contains(&b) {
                        self.state = StripState::Plain;
                    }
                }
                StripState::Csi => {
                    if b == 0x1b {
                        self.state = StripState::EscSeen;
                    } else if b == 0x18 || b == 0x1a {
                        // CAN/SUB cancel the sequence. Without this the next
                        // ordinary character was consumed as the CSI final
                        // byte: `ESC [ 31 CAN hello` logged `ello` while the
                        // terminal rendered `hello`.
                        self.state = StripState::Plain;
                    } else if (0x40..=0x7e).contains(&b) {
                        self.state = StripState::Plain;
                    }
                    // Else still inside CSI params — keep scanning.
                }
                StripState::Osc => {
                    if b == 0x07 || b == 0x9c {
                        self.state = StripState::Plain; // BEL terminator
                    } else if b == 0x18 || b == 0x1a {
                        // CAN/SUB cancel the string (DEC). Without this an
                        // unterminated OSC swallowed the remainder of the log.
                        self.state = StripState::Plain;
                    } else if b == 0x1b {
                        // ESC terminates OSC and begins a fresh escape. This is
                        // also how ESC \ represents ST.
                        self.state = StripState::EscSeen;
                    }
                    // Else still inside the OSC payload — keep scanning.
                }
                StripState::String => {
                    if b == 0x9c {
                        self.state = StripState::Plain; // 8-bit ST
                    } else if b == 0x18 || b == 0x1a {
                        self.state = StripState::Plain; // cancelled
                    } else if b == 0x1b {
                        self.state = StripState::EscSeen;
                    }
                    // Else still inside the payload — dropped, not logged.
                }
            }
        }
        out
    }

    fn escape_follower(b: u8) -> StripState {
        match b {
            b'[' => StripState::Csi,
            b']' => StripState::Osc,
            b'P' | b'X' | b'^' | b'_' => StripState::String,
            0x20..=0x2f => StripState::EscapeIntermediate,
            0x1b => StripState::EscSeen,
            _ => StripState::Plain,
        }
    }
}

#[derive(Default)]
struct SessionLogFilter {
    stripper: AnsiStripper,
    observed: Option<(u64, bool)>,
}

impl SessionLogFilter {
    fn filter(&mut self, input: &[u8], generation: u64, strip: bool) -> Vec<u8> {
        let current = (generation, strip);
        if self.observed != Some(current) {
            self.stripper = AnsiStripper::new();
            self.observed = Some(current);
        }
        if strip {
            self.stripper.strip(input)
        } else {
            input.to_vec()
        }
    }
}

/// Stateless one-shot ANSI strip: equivalent to `AnsiStripper::default()`
/// fed `input` once. Correct as long as every CSI/OSC sequence in `input`
/// is fully contained in this one call — a bare ESC or a sequence whose
/// terminator hasn't arrived yet is simply dropped, with no memory carried
/// to a subsequent call. Callers that strip a stream in chunks (e.g. the
/// per-pane session log, fed one PTY read at a time) MUST use
/// [`AnsiStripper`] directly and reuse the same instance across chunks, or a
/// sequence split across a chunk boundary leaks its continuation bytes into
/// the output as literal text.
pub fn strip_ansi_bytes(input: &[u8]) -> Vec<u8> {
    AnsiStripper::default().strip(input)
}

/// The kitty image-id bits a placeholder cell's foreground color carries:
/// a 256-palette index is the low byte, a truecolor spec is the low 24
/// bits, and the 16 ANSI named colors map to indices 0..=15
/// (`graphics-protocol.rst:589`). Non-id named slots (default fg/bg/cursor)
/// have no id → 0.
fn fg_id_bits(c: AnsiColor) -> u32 {
    use NamedColor::*;
    match c {
        AnsiColor::Indexed(i) => i as u32,
        AnsiColor::Spec(rgb) => ((rgb.r as u32) << 16) | ((rgb.g as u32) << 8) | rgb.b as u32,
        AnsiColor::Named(n) => match n {
            Black => 0,
            Red => 1,
            Green => 2,
            Yellow => 3,
            Blue => 4,
            Magenta => 5,
            Cyan => 6,
            White => 7,
            BrightBlack | DimBlack => 8,
            BrightRed | DimRed => 9,
            BrightGreen | DimGreen => 10,
            BrightYellow | DimYellow => 11,
            BrightBlue | DimBlue => 12,
            BrightMagenta | DimMagenta => 13,
            BrightCyan | DimCyan => 14,
            BrightWhite | DimWhite => 15,
            _ => 0,
        },
    }
}

fn placeholder_tile_placement(
    abs_line: u64,
    col: usize,
    resolved: placeholder::ResolvedCell,
    virtual_image: &VirtualEntry,
) -> Option<Placement> {
    let pcols = virtual_image.cols.max(1).min(u16::MAX as u32) as u16;
    let prows = virtual_image.rows.max(1).min(u16::MAX as u32) as u16;
    let (x, y, width, height) = placeholder::tile_src_rect(
        virtual_image.img.width,
        virtual_image.img.height,
        pcols,
        prows,
        resolved.row,
        resolved.col,
    )?;
    Some(Placement {
        abs_line,
        col,
        cell_cols: 1,
        cell_rows: 1,
        x_offset_cells: 0.0,
        y_offset_cells: 0.0,
        display_cols: 1.0,
        display_rows: 1.0,
        // Keep the original allocation shared across every placeholder cell.
        // The renderer samples only `source_rect`, avoiding a crop allocation
        // and a distinct GPU texture for every visible tile on every frame.
        img: virtual_image.img.clone(),
        source_rect: Some(ImageSourceRect {
            x,
            y,
            width,
            height,
        }),
        source_crop: None,
        id: Some(resolved.image_id),
        placement_id: virtual_image.placement_id,
        kitty_params: None,
        z: virtual_image.z,
    })
}

#[derive(Clone, Copy)]
struct GraphicsActionContext<'a> {
    images: &'a Images,
    virtuals: &'a Virtuals,
    anims: &'a Animations,
    relatives: &'a Relatives,
    geometry: &'a Arc<Mutex<VersionedPtyGeometry>>,
}

fn apply_kitty_delete_at(
    term: &Term<EventProxy>,
    delete: KittyDelete,
    context: GraphicsActionContext<'_>,
    extractor: &mut Extractor,
) {
    let grid = term.grid();
    let delete_geometry = KittyDeleteGeometry {
        screen_top: stable_grid_line_id(grid.history_origin(), grid.history_size(), 0),
        screen_lines: grid.screen_lines(),
        cursor_abs_line: stable_grid_line_id(
            grid.history_origin(),
            grid.history_size(),
            grid.cursor.point.line.0,
        ),
        cursor_col: grid.cursor.point.column.0,
    };
    let placeholder_cells = Terminal::placeholder_cells_from_term(term);
    let render_geometry = context
        .geometry
        .lock()
        .map(|geometry| geometry.geometry)
        .unwrap_or_else(|_| PtyGeometry::new(1, 1, 1, 1));

    // Resolve relative-placement origins before mutating any registry.
    let image_snapshot = context
        .images
        .lock()
        .map(|placements| placements.clone())
        .unwrap_or_default();
    let relative_snapshot = context
        .relatives
        .lock()
        .map(|placements| placements.clone())
        .unwrap_or_default();
    let mut origins = std::collections::HashMap::<u32, (u64, usize)>::new();
    let mut note_origin = |id: u32, abs: u64, col: usize| {
        origins
            .entry(id)
            .and_modify(|origin| {
                origin.0 = origin.0.min(abs);
                origin.1 = origin.1.min(col);
            })
            .or_insert((abs, col));
    };
    for placement in &image_snapshot {
        if let Some(id) = placement.id {
            note_origin(id, placement.abs_line, placement.col);
        }
    }
    for (abs, col, resolved) in &placeholder_cells {
        note_origin(resolved.image_id, *abs, *col);
    }
    let relative_chains = relative_snapshot
        .iter()
        .map(|(&(id, _), entry)| (id, (entry.parent_img, entry.h, entry.v)))
        .collect::<std::collections::HashMap<_, _>>();
    let relative_positions = relative_snapshot
        .iter()
        .filter_map(|(&(id, placement_id), entry)| {
            let (parent_abs, parent_col) =
                resolve_chain(entry.parent_img, &relative_chains, &origins, 8)?;
            let (abs_line, col) = relative_origin(parent_abs, parent_col, entry.h, entry.v);
            let resolved = resolve_kitty_placement(&entry.img, entry.params, render_geometry)?;
            Some((
                (id, placement_id),
                Placement {
                    abs_line,
                    col,
                    cell_cols: resolved.cell_cols,
                    cell_rows: resolved.cell_rows,
                    x_offset_cells: resolved.x_offset_cells,
                    y_offset_cells: resolved.y_offset_cells,
                    display_cols: resolved.display_cols,
                    display_rows: resolved.display_rows,
                    img: entry.img.clone(),
                    source_rect: resolved.source_rect,
                    source_crop: None,
                    id: Some(id),
                    placement_id,
                    kitty_params: Some(entry.params),
                    z: entry.z,
                },
            ))
        })
        .collect::<std::collections::HashMap<_, _>>();

    let mut removed_keys = std::collections::HashSet::<PlacementKey>::new();
    let mut removed_ids = std::collections::HashSet::<u32>::new();
    if let Ok(mut placements) = context.images.lock() {
        placements.retain(|placement| {
            if kitty_delete_matches_placement(&delete, placement, delete_geometry) {
                if let Some(image_id) = placement.id {
                    removed_ids.insert(image_id);
                    removed_keys.insert(PlacementKey {
                        image_id,
                        placement_id: placement.placement_id,
                    });
                }
                false
            } else {
                true
            }
        });
    }
    if let Ok(mut virtual_placements) = context.virtuals.lock() {
        virtual_placements.retain(|&(image_id, placement_id), _| {
            if kitty_delete_matches_virtual(&delete, image_id, placement_id) {
                removed_ids.insert(image_id);
                removed_keys.insert(PlacementKey {
                    image_id,
                    placement_id,
                });
                false
            } else {
                true
            }
        });
    }
    if let Ok(mut relative_placements) = context.relatives.lock() {
        relative_placements.retain(|&(image_id, placement_id), _| {
            let matched = relative_positions
                .get(&(image_id, placement_id))
                .is_some_and(|placement| {
                    kitty_delete_matches_placement(&delete, placement, delete_geometry)
                })
                || kitty_delete_matches_virtual(&delete, image_id, placement_id);
            if matched {
                removed_ids.insert(image_id);
                removed_keys.insert(PlacementKey {
                    image_id,
                    placement_id,
                });
                false
            } else {
                true
            }
        });

        // A relative placement cannot survive deletion of its concrete parent.
        loop {
            let before = relative_placements.len();
            relative_placements.retain(|&(image_id, placement_id), entry| {
                let parent_removed = removed_keys.iter().any(|key| {
                    key.image_id == entry.parent_img
                        && (entry.parent_placement == 0
                            || key.placement_id == entry.parent_placement)
                });
                if parent_removed {
                    removed_ids.insert(image_id);
                    removed_keys.insert(PlacementKey {
                        image_id,
                        placement_id,
                    });
                    false
                } else {
                    true
                }
            });
            if relative_placements.len() == before {
                break;
            }
        }
    }

    let mut freed_ids = Vec::new();
    if delete.free_data {
        removed_ids.extend(delete.free_candidates.iter().copied());
        let referenced = {
            let mut ids = std::collections::HashSet::<u32>::new();
            if let Ok(placements) = context.images.lock() {
                ids.extend(placements.iter().filter_map(|placement| placement.id));
            }
            if let Ok(virtual_placements) = context.virtuals.lock() {
                ids.extend(virtual_placements.keys().map(|&(id, _)| id));
            }
            if let Ok(relative_placements) = context.relatives.lock() {
                ids.extend(relative_placements.keys().map(|&(id, _)| id));
            }
            ids
        };
        freed_ids = kitty_delete_freed_ids(&delete, &removed_ids, &referenced);
        if let Ok(mut animations) = context.anims.lock() {
            for id in &freed_ids {
                animations.remove(id);
            }
        }
    }
    extractor.apply_kitty_delete_result(&removed_keys.into_iter().collect::<Vec<_>>(), &freed_ids);
}

fn apply_graphics_chunk_at(
    term: &mut Term<EventProxy>,
    chunk: Chunk,
    context: GraphicsActionContext<'_>,
    extractor: &mut Extractor,
) -> bool {
    match chunk {
        Chunk::Image(placed) => {
            let geometry = context
                .geometry
                .lock()
                .map(|geometry| geometry.geometry)
                .unwrap_or_else(|_| PtyGeometry::new(1, 1, 1, 1));
            place_image_during_sync(term, context.images, geometry, placed);
        }
        Chunk::DeleteImages(delete) => apply_kitty_delete_at(term, delete, context, extractor),
        Chunk::RelativePlacement {
            id,
            placement,
            img,
            parent_img,
            parent_placement,
            h,
            v,
            z,
            params,
        } => {
            if let Ok(mut relative_placements) = context.relatives.lock() {
                let key = (id, placement);
                let limit = kettle_vt::GraphicsLimits::default().placements;
                if relative_placements.contains_key(&key) || relative_placements.len() < limit {
                    relative_placements.insert(
                        key,
                        RelEntry {
                            img,
                            parent_img,
                            parent_placement,
                            h,
                            v,
                            z,
                            params,
                        },
                    );
                }
            }
        }
        Chunk::VirtualImage {
            id,
            placement,
            img,
            cols,
            rows,
            z,
        } => {
            if let Ok(mut virtual_placements) = context.virtuals.lock() {
                let limit = kettle_vt::GraphicsLimits::default().placements;
                let key = (id, placement);
                if virtual_placements.contains_key(&key) || virtual_placements.len() < limit {
                    virtual_placements.insert(
                        key,
                        VirtualEntry {
                            img,
                            placement_id: placement,
                            cols,
                            rows,
                            z,
                        },
                    );
                }
            }
        }
        Chunk::Animation {
            id,
            imgs,
            gaps,
            state,
        } => {
            if let Ok(mut animations) = context.anims.lock() {
                if imgs.len() <= 1 && !state.running {
                    animations.remove(&id);
                } else {
                    let started = match animations.get(&id) {
                        Some(previous) if previous.state.running == state.running => {
                            previous.started
                        }
                        _ => std::time::Instant::now(),
                    };
                    let limits = kettle_vt::GraphicsLimits::default();
                    let bytes = imgs
                        .iter()
                        .try_fold(0usize, |bytes, image| bytes.checked_add(image.byte_len()));
                    if (animations.contains_key(&id) || animations.len() < limits.placements)
                        && imgs.len() <= limits.animation_frames.saturating_add(1)
                        && limits
                            .animation_bytes
                            .checked_add(limits.image_bytes)
                            .zip(bytes)
                            .is_some_and(|(cap, bytes)| bytes <= cap)
                    {
                        animations.insert(
                            id,
                            AnimEntry {
                                imgs,
                                gaps,
                                state,
                                started,
                            },
                        );
                    }
                }
            }
        }
        _ => return false,
    }
    true
}

/// Anchor a decoded image at the application cursor. Kitty advances right by
/// the effective columns and down by rows minus one unless `C=1`; the legacy
/// iTerm2/Sixel path retains Kettle's line-reservation policy.
fn insert_image_at_cursor(
    term: &mut Term<EventProxy>,
    images: &Images,
    geometry: PtyGeometry,
    placed: kettle_vt::Placed,
) -> Option<(Option<PlacementParams>, usize, usize)> {
    let kettle_vt::Placed {
        img: data,
        id,
        placement_id,
        z,
        params,
    } = placed;
    let resolved = if let Some(params) = params {
        resolve_kitty_placement(&data, params, geometry)?
    } else {
        let cell_cols = image_cells_for_pixels(data.width, geometry.columns, geometry.pixel_width);
        let cell_rows = image_cells_for_pixels(data.height, geometry.rows, geometry.pixel_height);
        ResolvedKittyPlacement {
            source_rect: None,
            cell_cols,
            cell_rows,
            x_offset_cells: 0.0,
            y_offset_cells: 0.0,
            display_cols: cell_cols as f32,
            display_rows: cell_rows as f32,
        }
    };
    let cell_cols = resolved.cell_cols;
    let cell_rows = resolved.cell_rows;
    let cursor = term.grid().cursor.point;
    let abs_line = stable_grid_line_id(
        term.grid().history_origin(),
        term.grid().history_size(),
        cursor.line.0,
    );
    if let Ok(mut placements) = images.lock() {
        if let Some(id) = id
            && placement_id != 0
        {
            placements.retain(|placement| {
                placement.id != Some(id) || placement.placement_id != placement_id
            });
        }
        placements.push(Placement {
            abs_line,
            col: cursor.column.0,
            cell_cols,
            cell_rows,
            x_offset_cells: resolved.x_offset_cells,
            y_offset_cells: resolved.y_offset_cells,
            display_cols: resolved.display_cols,
            display_rows: resolved.display_rows,
            img: data,
            source_rect: resolved.source_rect,
            source_crop: None,
            id,
            placement_id,
            kitty_params: params,
            z,
        });
        let limit = kettle_vt::GraphicsLimits::default().placements;
        if placements.len() > limit {
            let drop = placements.len() - limit;
            placements.drain(0..drop);
        }
    }
    Some((params, cell_cols, cell_rows))
}

fn place_image_during_sync(
    term: &mut Term<EventProxy>,
    images: &Images,
    geometry: PtyGeometry,
    placed: kettle_vt::Placed,
) {
    let Some((params, cell_cols, cell_rows)) =
        insert_image_at_cursor(term, images, geometry, placed)
    else {
        return;
    };
    if let Some(params) = params {
        if !params.suppress_cursor_movement {
            term.move_forward(cell_cols);
            term.move_down(cell_rows.saturating_sub(1));
        }
    } else {
        for _ in 0..cell_rows.clamp(1, 256) {
            term.carriage_return();
            term.linefeed();
        }
    }
}

fn place_image(
    term: &SharedTerm,
    images: &Images,
    geometry: &Arc<Mutex<VersionedPtyGeometry>>,
    processor: &mut Processor,
    placed: kettle_vt::Placed,
) -> Option<GraphicsEventBatch> {
    // Match resize's Term -> geometry lock order. Holding both through the
    // cursor snapshot and row reservation makes the grid/pixel generation one
    // atomic observation for image placement.
    let mut t = term.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let geometry = geometry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let geometry = geometry.geometry;
    let (params, cell_cols, cell_rows) = insert_image_at_cursor(&mut t, images, geometry, placed)?;
    let mut advanced = false;
    if let Some(params) = params {
        if let Some(movement) = kitty_cursor_movement(params, cell_cols, cell_rows) {
            processor.advance(&mut *t, movement.as_bytes());
            advanced = true;
        }
    } else {
        // Legacy iTerm2/Sixel policy: reserve rows by emitting line breaks.
        let nl = "\r\n".repeat(cell_rows.clamp(1, 256));
        processor.advance(&mut *t, nl.as_bytes());
        advanced = true;
    }
    advanced.then(|| t.take_graphics_events())
}

#[cfg(test)]
mod cwd_reporting_tests {
    use super::reported_current_dir;

    #[test]
    fn launch_seed_is_not_reported_until_an_osc_cwd_arrives() {
        assert_eq!(
            reported_current_dir(false, Some("C:\\launch-seed".to_owned())),
            None
        );
        assert_eq!(
            reported_current_dir(true, Some("/shell/reported".to_owned())),
            Some("/shell/reported".to_owned())
        );
    }
}

/// The PTY reader holds `term` and then takes `virtuals` to replay a deferred
/// kitty virtual chunk. Any render path that holds `virtuals` while acquiring
/// `term` is an ABBA deadlock: a child emitting `CSI ? 2026 h`, a deferred
/// virtual placement, placeholder cells and `CSI ? 2026 l` while the UI paints
/// parks both threads forever and freezes the pane. `placeholder_tiles` used to
/// hold `virtuals` across a `placeholder_cells` call, which takes `term`.
///
/// A behavioural test cannot force that interleaving deterministically, and no
/// source-text guard can prove a `MutexGuard` was dropped — brace depth, `};`
/// searches and call ordering all pass on a body that moves the guard out of
/// its block and keeps it live. So the release is enforced by the type system
/// instead: `placeholder_tiles` gets its data from `virtuals_snapshot`, whose
/// owned return type no guard can escape through. That much is a compile error
/// to break, not a review note.
///
/// **What the test below does and does not cover**, because a guard that
/// overstates its reach is how this function acquired the bug twice. It pins
/// four things: `placeholder_tiles` contains no `virtuals.lock()` of its own;
/// it consults the snapshot before the grid read; `virtuals_snapshot` is the
/// thing that locks; and that helper's signature still returns owned maps.
///
/// It is text matching. It cannot see a lock taken through indirection that
/// never writes `virtuals.lock()` — a macro expanding to `$mutex.lock()`, a
/// `let mutex = self.virtuals.as_ref()` rebind, or a helper taking
/// `&Virtuals` and handing back a guard. Nor can it be a count of lock sites:
/// the PTY reader locks `virtuals` in six other places and is right to, since
/// it already holds `term` and so takes them in the safe order.
///
/// What actually rules out the deadlock in the code as written is the owned
/// return type — a `MutexGuard` cannot escape through it, and the compiler
/// enforces that. The test keeps `placeholder_tiles` pointed at that door;
/// a new lock reached by indirection is review's job, not this file's.
#[cfg(test)]
mod placeholder_lock_order_tests {
    #[test]
    fn placeholder_tiles_reaches_virtuals_only_through_the_owned_snapshot() {
        // Normalized, like every other source guard in this file: the split
        // patterns below embed `\n`, so a CRLF checkout would silently find
        // nothing and fail on an unrelated-looking `expect`.
        let src = super::production_source();
        let body = src
            .split("pub fn placeholder_tiles(&self) -> Vec<Placement> {")
            .nth(1)
            .and_then(|rest| rest.split("\n    pub fn ").next())
            .expect("placeholder_tiles body");

        assert!(
            !body.contains("virtuals.lock()"),
            "placeholder_tiles must not lock `virtuals` itself; taking the \
             guard here lets a later edit hold it across `placeholder_cells`, \
             which is the ABBA deadlock the reader's `term` -> `virtuals` \
             order creates"
        );

        // `virtuals_snapshot` must be the thing that locks — otherwise the
        // assertion above passes on a `placeholder_tiles` that gets its guard
        // from somewhere else and the owned signature guards nothing.
        //
        // Deliberately not a count over the module: the PTY reader locks
        // `virtuals` in six other places and is right to, because it already
        // holds `term` and so takes them in the safe order. A "exactly one lock
        // site" assertion reads well and is simply false about this file.
        let snapshot_body = src
            .split("fn virtuals_snapshot(")
            .nth(1)
            .and_then(|rest| rest.split("\n    fn ").next())
            .and_then(|rest| rest.split("\n    pub fn ").next())
            .expect("virtuals_snapshot body");
        assert!(
            snapshot_body.contains("virtuals.lock()"),
            "virtuals_snapshot must be the lock site `placeholder_tiles` goes \
             through; if it stops locking, the owned signature below is \
             guarding nothing"
        );

        let snapshot_at = body
            .find("self.virtuals_snapshot()")
            .expect("placeholder_tiles must consult the virtual map");
        let cells_at = body
            .find("self.placeholder_cells()")
            .expect("placeholder_tiles must read the grid");
        assert!(
            snapshot_at < cells_at,
            "placeholder_tiles must consult `virtuals` before walking the grid; \
             otherwise every pane pays a full visible-cell scan per frame to \
             discover it has no virtual placements"
        );

        // The owned signature is what makes the release a compile error rather
        // than a review note. Pin it: widening the return type to borrow from
        // the guard would silently restore the deadlock.
        // All whitespace removed, and the trailing comma rustfmt only emits in
        // the wrapped form dropped, so the signature may be rewrapped freely.
        let signature = src
            .split("fn virtuals_snapshot(")
            .nth(1)
            .and_then(|rest| rest.split(" {\n").next())
            .expect("virtuals_snapshot signature")
            .split_whitespace()
            .collect::<String>()
            .replace(",)", ")");
        assert_eq!(
            signature, "&self)->Option<(HashMap<(u32,u32),VirtualEntry>,HashMap<u32,u32>)>",
            "virtuals_snapshot must keep returning owned maps; a borrowed \
             return type would let the `virtuals` guard escape to a caller \
             that then locks `term`"
        );
    }

    /// `relative_tiles` is the in-repo precedent: snapshot under one lock, drop
    /// it, then take the others. Keep its comment honest so the discipline is
    /// discoverable from either function.
    #[test]
    fn relative_tiles_still_documents_the_single_acquisition_order() {
        // Normalized, like every other source guard in this file: the split
        // patterns below embed `\n`, so a CRLF checkout would silently find
        // nothing and fail on an unrelated-looking `expect`.
        let src = super::production_source();
        let body = src
            .split("pub fn relative_tiles(&self) -> Vec<Placement> {")
            .nth(1)
            .and_then(|rest| rest.split("\n    pub fn ").next())
            .expect("relative_tiles body");
        assert!(
            body.contains("single lock-acquisition order"),
            "relative_tiles must keep stating why it snapshots before locking"
        );
    }
}

#[cfg(test)]
mod placeholder_tile_placement_tests {
    use super::{VirtualEntry, placeholder_tile_placement};
    use kettle_vt::ImageData;
    use kettle_vt::placeholder::ResolvedCell;

    #[test]
    fn placeholder_tile_shares_pixels_and_records_source_rect() {
        let image = ImageData::new(4, 2, vec![0; 4 * 2 * 4]).expect("test image");
        let virtual_image = VirtualEntry {
            img: image.clone(),
            placement_id: 0,
            cols: 2,
            rows: 1,
            z: 7,
        };
        let placement = placeholder_tile_placement(
            12,
            3,
            ResolvedCell {
                image_id: 42,
                placement_id: 0,
                row: 0,
                col: 1,
            },
            &virtual_image,
        )
        .expect("right-hand tile");

        assert_eq!(placement.img.allocation_key(), image.allocation_key());
        assert_eq!(placement.img.byte_len(), image.byte_len());
        assert_eq!(
            placement.source_rect,
            Some(crate::ImageSourceRect {
                x: 2,
                y: 0,
                width: 2,
                height: 2,
            })
        );
    }
}

/// End-to-end VT conformance: drives the *same* parser path the PTY reader
/// uses (alacritty_terminal + vte) over a battery of escape sequences and
/// asserts the resulting grid/cursor/mode. This is the automatable,
/// regression-proof core of a `vttest` sweep.
#[cfg(test)]
mod detect_shells_tests {

    /// Drift guard. `list_wsl_distros` runs on the UI thread
    /// (new-tab `▾`), so its `wsl.exe` call must stay BOUNDED — a wedged
    /// LxssManager (the `Wsl/Service/E_UNEXPECTED` freeze) otherwise hangs the
    /// window. Pin the worker-thread + `recv_timeout` shape at the source level
    /// (a behavioral test would need to hang a real `wsl.exe`).
    #[test]
    fn list_wsl_distros_is_time_bounded() {
        let src = super::production_source();
        let start = src
            .find("fn list_wsl_distros()")
            .expect("list_wsl_distros present");
        let body = &src[start..start + 1200];
        assert!(
            body.contains("recv_timeout"),
            "list_wsl_distros must bound the wsl.exe call with recv_timeout so a \
             hung LxssManager can't freeze the UI thread"
        );
    }

    #[test]
    fn parse_wsl_distros_strips_bom_nul_blanks_crlf() {
        // Simulated `wsl -l -q` decoded text: a leading UTF-16 BOM, CRLF line
        // endings, a blank line, and a trailing-NUL artifact.
        let text = "\u{feff}Ubuntu\r\nDebian\r\n\r\nkali-linux\u{0}\r\n";
        assert_eq!(
            super::parse_wsl_distros(text),
            vec!["Ubuntu", "Debian", "kali-linux"]
        );
        assert!(super::parse_wsl_distros("").is_empty());
        assert!(super::parse_wsl_distros("\u{feff}\r\n  \r\n").is_empty());
    }

    /// Dropdown parity: the full Windows menu in Windows Terminal's
    /// order, with every probe succeeding. Runs on every platform — the
    /// builder is pure over injected closures.
    #[test]
    fn detect_shells_windows_orders_like_windows_terminal() {
        let avail = |e: &str| matches!(e, "cmd.exe" | "powershell.exe" | "pwsh.exe" | "wsl.exe");
        let distros = || vec!["Ubuntu".to_string()];
        let vs = || {
            Some(super::VsDevInfo {
                install_path: r"C:\Program Files\Microsoft Visual Studio\2022\Community"
                    .to_string(),
                has_dev_cmd_bat: true,
                has_dev_shell_dll: true,
            })
        };
        let git = || {
            Some(std::path::PathBuf::from(
                r"C:\Program Files\Git\bin\bash.exe",
            ))
        };
        let got = super::detect_shells_windows(avail, distros, vs, git);
        let labels: Vec<&str> = got.iter().map(|(l, _)| l.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "PowerShell",
                "Windows PowerShell",
                "Command Prompt",
                "WSL: Ubuntu",
                "Developer Command Prompt for VS 2022",
                "Developer PowerShell for VS 2022",
                "Git Bash",
            ],
            "Windows Terminal's dropdown order (Ctrl+Shift+N indexes this)"
        );
        // Git Bash spawns an interactive login shell from its full path.
        let (_, bash_argv) = got.iter().find(|(l, _)| l == "Git Bash").unwrap();
        assert_eq!(
            bash_argv.as_slice(),
            [r"C:\Program Files\Git\bin\bash.exe", "-i", "-l"]
        );
    }

    /// Dropdown parity: hosts without VS / Git get no phantom rows.
    #[test]
    fn detect_shells_windows_skips_vs_and_git_when_absent() {
        let avail = |e: &str| matches!(e, "cmd.exe" | "pwsh.exe" | "wsl.exe");
        let got = super::detect_shells_windows(avail, || vec!["Ubuntu".into()], || None, || None);
        assert!(got.iter().any(|(l, _)| l == "PowerShell"));
        assert!(got.iter().any(|(l, _)| l == "WSL: Ubuntu"));
        // powershell.exe was NOT "available" → Windows PowerShell absent.
        assert!(!got.iter().any(|(l, _)| l == "Windows PowerShell"));
        assert!(!got.iter().any(|(l, _)| l.starts_with("Developer")));
        assert!(!got.iter().any(|(l, _)| l == "Git Bash"));
    }

    /// Dropdown parity: the Developer PowerShell host prefers pwsh 7
    /// and falls back to Windows PowerShell.
    #[test]
    fn vs_dev_powershell_host_prefers_pwsh() {
        let vs = || {
            Some(super::VsDevInfo {
                install_path: r"C:\VS\2022\BuildTools".to_string(),
                has_dev_cmd_bat: false,
                has_dev_shell_dll: true,
            })
        };
        // Only powershell.exe on PATH → it hosts the dev shell.
        let got = super::detect_shells_windows(|e| e == "powershell.exe", Vec::new, vs, || None);
        let (_, argv) = got
            .iter()
            .find(|(l, _)| l.starts_with("Developer PowerShell"))
            .unwrap();
        assert_eq!(argv[0], "powershell.exe");
    }

    /// Dropdown parity: the dev-shell argvs are byte-pinned — these
    /// strings are what actually spawns, so a drift here breaks the feature
    /// invisibly (the menu row would still render).
    #[test]
    fn vs_dev_argv_strings_are_canonical() {
        let argv =
            super::vs_dev_cmd_argv(r"C:\Program Files\Microsoft Visual Studio\2022\Community");
        assert_eq!(
            argv.as_slice(),
            [
                "cmd.exe",
                "/k",
                r"C:\Program Files\Microsoft Visual Studio\2022\Community\Common7\Tools\VsDevCmd.bat",
            ]
        );
        let argv = super::vs_dev_powershell_argv("pwsh.exe", r"C:\VS\O'Brien\2022\Community");
        assert_eq!(argv[0], "pwsh.exe");
        assert_eq!(argv[1], "-NoExit");
        assert_eq!(argv[2], "-Command");
        // Single quotes in the path are doubled (PowerShell escaping); the
        // command imports DevShell.dll then enters the dev environment
        // without changing directory.
        assert_eq!(
            argv[3],
            r"&{ Import-Module 'C:\VS\O''Brien\2022\Community\Common7\Tools\Microsoft.VisualStudio.DevShell.dll'; Enter-VsDevShell -VsInstallPath 'C:\VS\O''Brien\2022\Community' -SkipAutomaticLocation }"
        );
    }

    #[test]
    fn parse_vs_install_path_takes_first_nonempty_line() {
        assert_eq!(
            super::parse_vs_install_path("\r\n C:\\VS\\2022\\Community \r\n"),
            Some(r"C:\VS\2022\Community".to_string())
        );
        assert_eq!(super::parse_vs_install_path("\n  \n"), None);
    }

    #[test]
    fn vs_year_from_install_path_finds_the_year_segment() {
        assert_eq!(
            super::vs_year_from_install_path(
                r"C:\Program Files\Microsoft Visual Studio\2022\Community"
            ),
            Some("2022")
        );
        assert_eq!(
            super::vs_year_from_install_path("/c/vs/2026/Preview"),
            Some("2026")
        );
        assert_eq!(super::vs_year_from_install_path(r"C:\VS\Preview"), None);
    }

    // Windows-only: `git_bash_from_git_exe` reasons over `std::path::Path`,
    // and `\` is only a separator on Windows targets — on Linux these
    // fixtures parse as single opaque components (the production caller,
    // `git_bash_path`, is `cfg(windows)` for the same reason).
    #[cfg(windows)]
    #[test]
    fn git_bash_from_git_exe_covers_cmd_bin_and_mingw_layouts() {
        use std::path::{Path, PathBuf};
        // <root>\cmd\git.exe → <root>\bin\bash.exe
        let c = super::git_bash_from_git_exe(Path::new(r"C:\Git\cmd\git.exe"));
        assert_eq!(c, vec![PathBuf::from(r"C:\Git\bin\bash.exe")]);
        // <root>\mingw64\bin\git.exe → <root>\bin\bash.exe (+ sibling)
        let c = super::git_bash_from_git_exe(Path::new(r"C:\Git\mingw64\bin\git.exe"));
        assert!(c.contains(&PathBuf::from(r"C:\Git\bin\bash.exe")));
        // flat dir → sibling bash.exe
        let c = super::git_bash_from_git_exe(Path::new(r"D:\tools\git.exe"));
        assert_eq!(c, vec![PathBuf::from(r"D:\tools\bash.exe")]);
    }

    #[cfg(windows)]
    #[test]
    fn detect_shells_windows_never_empty() {
        let got = super::detect_shells_windows(|_| false, Vec::new, || None, || None);
        assert_eq!(
            got,
            vec![("Command Prompt".to_string(), vec!["cmd.exe".to_string()])]
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn detect_shells_unix_shell_env_first_and_dedupes() {
        let avail = |e: &str| matches!(e, "bash" | "zsh" | "fish");
        let got = super::detect_shells_unix(Some("/bin/zsh".to_string()), avail);
        // $SHELL=zsh is first (label = basename); the detected `zsh` isn't a dup.
        assert_eq!(got[0], ("zsh".to_string(), vec!["/bin/zsh".to_string()]));
        assert_eq!(got.iter().filter(|(_, a)| a[0].ends_with("zsh")).count(), 1);
        assert!(got.iter().any(|(l, _)| l == "bash"));
        assert!(got.iter().any(|(l, _)| l == "fish"));
    }

    #[cfg(not(windows))]
    #[test]
    fn detect_shells_unix_never_empty() {
        let got = super::detect_shells_unix(None, |_| false);
        assert_eq!(
            got,
            vec![("Shell".to_string(), vec!["/bin/sh".to_string()])]
        );
    }
}

#[cfg(test)]
mod wslenv_tests {
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
}

#[cfg(test)]
mod home_dir_tests {
    use super::{LazySessionLogWriter, home_dir_fallback};
    use crate::persistence::AsyncFileWriter;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::time::Duration;

    fn from<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<OsString> + 'a {
        move |k| {
            pairs
                .iter()
                .find(|(name, _)| *name == k)
                .map(|(_, v)| OsString::from(*v))
        }
    }

    #[test]
    fn prefers_home_then_userprofile_then_appdata() {
        // All three set (rare — a WSL user with both env worlds bleeding
        // through) → HOME wins. This is the Linux / macOS branch.
        assert_eq!(
            home_dir_fallback(from(&[
                ("HOME", "/h"),
                ("USERPROFILE", r"C:\u"),
                ("APPDATA", r"C:\a"),
            ])),
            Some(PathBuf::from("/h")),
        );
        // No HOME → USERPROFILE. This is the *Windows* branch — exactly
        // the gap the previous `var_os("HOME")`-only fallback missed.
        assert_eq!(
            home_dir_fallback(from(&[("USERPROFILE", r"C:\u"), ("APPDATA", r"C:\a"),])),
            Some(PathBuf::from(r"C:\u")),
        );
        // Only APPDATA set (very stripped Windows session) → APPDATA.
        assert_eq!(
            home_dir_fallback(from(&[("APPDATA", r"C:\a")])),
            Some(PathBuf::from(r"C:\a")),
        );
        // Nothing set (minimal Linux container without HOME) → None;
        // caller leaves cmd.cwd() untouched.
        assert_eq!(home_dir_fallback(from(&[])), None);
    }

    #[test]
    fn empty_env_var_value_falls_through_to_next() {
        // `HOME=""` (a deliberately empty env var — happens
        // in stripped-down CI containers and after a misconfigured
        // `unset HOME` / `export HOME=` in a parent shell) used to
        // return `Some(PathBuf::from(""))`. CommandBuilder::cwd("")
        // then fed an invalid empty path to the OS spawn. Now empty
        // values are filtered as if unset, so the probe continues to
        // the next variable. Pinned at every level of the chain.
        //
        // HOME empty, USERPROFILE valid → USERPROFILE wins.
        assert_eq!(
            home_dir_fallback(from(&[("HOME", ""), ("USERPROFILE", r"C:\u")])),
            Some(PathBuf::from(r"C:\u")),
        );
        // HOME empty, USERPROFILE empty, APPDATA valid → APPDATA wins.
        assert_eq!(
            home_dir_fallback(from(&[
                ("HOME", ""),
                ("USERPROFILE", ""),
                ("APPDATA", r"C:\a"),
            ])),
            Some(PathBuf::from(r"C:\a")),
        );
        // All three empty → None. Caller leaves cmd.cwd() untouched
        // rather than handing an empty path to the OS spawn.
        assert_eq!(
            home_dir_fallback(from(&[("HOME", ""), ("USERPROFILE", ""), ("APPDATA", ""),])),
            None,
        );
    }

    /// Drift guard. `strip_ansi_bytes` is the pure ANSI-
    /// strip helper behind `log_strip_ansi`. Verify:
    ///   - CSI sequences (SGR / cursor moves / etc.) are removed
    ///   - OSC sequences (title, hyperlink, OSC 7) are removed,
    ///     terminated by either BEL or ESC\
    ///   - Single-char ESC (ESC c full-reset) is removed
    ///   - Plain printable bytes + newlines pass through
    #[test]
    fn strip_ansi_bytes_removes_csi_osc_and_single_esc() {
        use super::strip_ansi_bytes;
        // CSI SGR around plain text: "hello world".
        let s = b"\x1b[31mhello\x1b[0m world";
        assert_eq!(strip_ansi_bytes(s), b"hello world");
        // OSC 0 (set title) terminated by BEL.
        let s = b"prefix \x1b]0;my-title\x07 suffix";
        assert_eq!(strip_ansi_bytes(s), b"prefix  suffix");
        // OSC 8 (hyperlink) terminated by ESC\\.
        let s = b"\x1b]8;;http://example/\x1b\\link text\x1b]8;;\x1b\\";
        assert_eq!(strip_ansi_bytes(s), b"link text");
        // Single-char ESC (full reset).
        let s = b"\x1bcclean";
        assert_eq!(strip_ansi_bytes(s), b"clean");
        // Newlines + tabs pass through.
        let s = b"line1\nline2\tindent\n";
        assert_eq!(strip_ansi_bytes(s), b"line1\nline2\tindent\n");
        // Bare ESC at the very end of buffer is dropped (matches
        // the documented split-across-reads limitation).
        let s = b"trail\x1b";
        assert_eq!(strip_ansi_bytes(s), b"trail");
        // Plain ASCII passes through unchanged.
        let s = b"no escapes here";
        assert_eq!(strip_ansi_bytes(s), b"no escapes here");
    }

    #[test]
    fn session_log_parser_tap_only_uses_bounded_worker_admission() {
        let source = super::production_source();
        let tap = source
            .split("extractor.set_raw_tap(tap_raw);")
            .nth(1)
            .and_then(|body| body.split("image_pruner.prune_if_changed").next())
            .expect("session-log parser tap");
        assert!(
            tap.contains("writer.try_write(filtered)"),
            "the parser must hand log chunks to the shared persistence worker"
        );
        assert!(
            !tap.contains("write_all") && !tap.contains(".flush("),
            "filesystem write and flush calls must stay off the parser thread"
        );
    }

    #[test]
    fn session_log_target_open_is_deferred_to_the_persistence_worker() {
        let temp = kettle_test_support::private_tempdir("kettle-session-log-test-");
        let path = temp.path().join("deferred-session.log");
        let sink = LazySessionLogWriter::new(path.clone());
        assert!(
            !path.exists(),
            "constructing the handoff must not touch the filesystem"
        );

        let mut writer = AsyncFileWriter::spawn("session-log-open-test", Box::new(sink)).unwrap();
        assert!(
            !path.exists(),
            "an idle persistence worker must not create the target before data arrives"
        );
        writer
            .try_write(b"native parser output\n".to_vec())
            .unwrap();
        assert!(
            writer.finish_with_timeout(Duration::from_secs(2)),
            "deferred private target failed with {:?}",
            writer.status()
        );
        assert_eq!(std::fs::read(&path).unwrap(), b"native parser output\n");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    /// Regression test for the split-mid-sequence log-corruption bug: a
    /// CSI/OSC sequence whose terminator lands in the *next* PTY-read chunk
    /// must still be recognized (and fully removed) when the same
    /// `AnsiStripper` instance is reused across calls — exactly how the
    /// reader thread's per-pane log path uses it. Each case below splits a
    /// The log stripper must agree with the terminal about where a sequence
    /// ends, or the log and the screen disagree about what happened.
    ///
    /// Three ways it did not:
    ///   * CAN/SUB did not cancel a CSI, so the next ordinary character was
    ///     eaten as the final byte — `ESC [ 31 CAN hello` logged `ello` while
    ///     the terminal rendered `hello`.
    ///   * A non-ST `ESC` inside a control string did not abort it, so
    ///     `ESC ^ payload ESC c visible` left the stripper inside the string
    ///     and swallowed everything after.
    ///   * `0x9c` is both the 8-bit ST and a UTF-8 continuation byte, so a
    ///     payload containing `末` (`e6 9c ab`) terminated at the middle of
    ///     that character and leaked its tail into the log.
    #[test]
    fn the_log_stripper_ends_sequences_where_the_terminal_does() {
        for (label, input, want) in [
            ("CAN cancels a CSI", &b"\x1b[31\x18hello"[..], "hello"),
            ("SUB cancels a CSI", &b"\x1b[31\x1ahello"[..], "hello"),
            (
                "an escape intermediate consumes its final",
                &b"before\x1b(Bafter"[..],
                "beforeafter",
            ),
            (
                "ESC from CSI starts a fresh OSC",
                &b"\x1b[31\x1b]0;secret\x07visible"[..],
                "visible",
            ),
            (
                "raw ST terminates OSC",
                &b"\x1b]0;title\x9cvisible"[..],
                "visible",
            ),
            (
                "a non-ST ESC aborts OSC",
                &b"\x1b]0;title\x1bcvisible"[..],
                "visible",
            ),
            (
                "a non-ST ESC aborts a control string",
                &b"\x1b^payload\x1bcvisible"[..],
                "visible",
            ),
            (
                "CAN after that ESC still cancels",
                &b"\x1b^payload\x1b\x18visible"[..],
                "visible",
            ),
            (
                "0x9c inside a UTF-8 character is not ST",
                "\x1b_G payload \u{672b} more\x1b\\after".as_bytes(),
                "after",
            ),
            (
                "and the same in an OSC",
                "\x1b]0;title \u{672b} more\x07after".as_bytes(),
                "after",
            ),
        ] {
            let mut stripper = super::AnsiStripper::new();
            let out = stripper.strip(input);
            assert_eq!(
                String::from_utf8_lossy(&out),
                want,
                "{label}: the stripper must end the sequence where the terminal does"
            );
        }
    }

    #[test]
    fn escaped_utf8_leads_do_not_emit_orphaned_continuations() {
        for scalar in [
            &b"\xc3\xa9"[..],
            &b"\xe2\x82\xac"[..],
            &b"\xf0\x9f\x99\x82"[..],
        ] {
            for split in 0..=scalar.len() {
                let mut input = b"head\x1b".to_vec();
                let scalar_start = input.len();
                input.extend_from_slice(scalar);
                input.extend_from_slice(b"tail");
                let mut stripper = super::AnsiStripper::new();
                let boundary = scalar_start + split;
                let mut out = stripper.strip(&input[..boundary]);
                out.extend(stripper.strip(&input[boundary..]));
                assert_eq!(
                    std::str::from_utf8(&out),
                    Ok("headtail"),
                    "scalar {scalar:?}, split {split}: {out:?}"
                );
            }
        }
    }

    #[test]
    fn session_log_filter_resets_between_sessions_and_strip_modes() {
        let mut filter = super::SessionLogFilter::default();
        assert_eq!(filter.filter(b"before\x1b]0;partial", 1, true), b"before");

        // Logging is inactive while the terminal consumes the BEL. A new log
        // must not inherit the old log's OSC state.
        assert_eq!(filter.filter(b"visible", 2, true), b"visible");

        // Changing raw/stripped mode is another parser-session boundary even
        // when the writer generation itself has not changed.
        assert_eq!(filter.filter(b"\x1b]0;partial", 2, true), b"");
        assert_eq!(filter.filter(b"raw", 2, false), b"raw");
        assert_eq!(filter.filter(b"visible", 2, true), b"visible");
    }

    /// Ordinary UTF-8 text must survive the stripper untouched — the log is
    /// meant to be readable.
    #[test]
    fn utf8_text_passes_through_the_log_stripper_unharmed() {
        for text in ["末端", "┌─┐ ‘quoted’ Ünicode └─┘", "🦀 crab", "Ûh"] {
            let mut stripper = super::AnsiStripper::new();
            let out = stripper.strip(text.as_bytes());
            assert_eq!(
                String::from_utf8(out).as_deref(),
                Ok(text),
                "plain text must reach the log byte for byte"
            );
        }
        // And across every chunk split, since the log is fed one PTY read at
        // a time.
        let text = "末端 ▐ ‘q’";
        let bytes = text.as_bytes();
        for split in 1..bytes.len() {
            let mut stripper = super::AnsiStripper::new();
            let mut out = stripper.strip(&bytes[..split]);
            out.extend_from_slice(&stripper.strip(&bytes[split..]));
            assert_eq!(
                String::from_utf8(out).as_deref(),
                Ok(text),
                "split at {split} corrupted the text"
            );
        }
    }

    /// A session log must not accumulate image payloads.
    ///
    /// DCS (`ESC P`, Sixel) and APC (`ESC _`, Kitty graphics) were treated as
    /// single-character escapes, so only the two introducer bytes were removed
    /// and the entire BODY was written to the log as text. Kitty payloads carry
    /// encoded file paths and shared-memory names, and Sixel carries raw pixel
    /// data — so a log the user enabled to keep a readable transcript was
    /// instead accumulating binary and, worse, path-bearing payloads.
    #[test]
    fn session_log_stripping_drops_image_payloads_not_just_their_introducers() {
        for (label, input, want) in [
            (
                "Sixel DCS",
                &b"before\x1bPq#0;2;0;0;0#0~~@@vv@@~~@@~~$\x1b\\after"[..],
                "beforeafter",
            ),
            (
                "Kitty APC with a file path",
                &b"before\x1b_Ga=T,f=100,t=f;L3RtcC9zZWNyZXQucG5n\x1b\\after"[..],
                "beforeafter",
            ),
            ("PM", &b"before\x1b^private\x1b\\after"[..], "beforeafter"),
            ("SOS", &b"before\x1bXstring\x1b\\after"[..], "beforeafter"),
            (
                "DCS closed by 8-bit ST",
                &b"before\x1bPpayload\x9cafter"[..],
                "beforeafter",
            ),
        ] {
            let mut stripper = super::AnsiStripper::new();
            let out = stripper.strip(input);
            assert_eq!(
                String::from_utf8_lossy(&out),
                want,
                "{label}: the payload must be dropped, not logged"
            );
        }
    }

    /// A control string split across chunks must stay suppressed — the log is
    /// fed one PTY read at a time, so a payload routinely straddles a boundary.
    #[test]
    fn a_control_string_split_across_log_chunks_stays_suppressed() {
        let input = b"before\x1b_Ga=T;payload-with-/tmp/path\x1b\\after";
        for split in 1..input.len() {
            let mut stripper = super::AnsiStripper::new();
            let mut out = stripper.strip(&input[..split]);
            out.extend_from_slice(&stripper.strip(&input[split..]));
            assert_eq!(
                String::from_utf8_lossy(&out),
                "beforeafter",
                "split at {split} leaked payload into the log"
            );
        }
    }

    /// CAN/SUB cancel a control string, so an unterminated one cannot swallow
    /// the rest of the log.
    #[test]
    fn a_cancelled_control_string_does_not_swallow_the_rest_of_the_log() {
        for cancel in [0x18_u8, 0x1a] {
            for intro in [&b"\x1b]0;"[..], &b"\x1bP"[..], &b"\x1b_"[..]] {
                let mut input = b"before".to_vec();
                input.extend_from_slice(intro);
                input.extend_from_slice(b"payload");
                input.push(cancel);
                input.extend_from_slice(b"after");

                let mut stripper = super::AnsiStripper::new();
                let out = stripper.strip(&input);
                assert_eq!(
                    String::from_utf8_lossy(&out),
                    "beforeafter",
                    "cancel {cancel:#04x} after {intro:?}: the log must resume"
                );
            }
        }
    }

    /// sequence at a different, deliberately awkward byte boundary; with the
    /// old per-call-stateless `strip_ansi_bytes` the second chunk would have
    /// leaked raw escape-sequence bytes into the log as literal text.
    #[test]
    fn ansi_stripper_persists_state_across_split_sequences() {
        use super::AnsiStripper;

        // CSI params split before the final byte: "\x1b[31mhello" split as
        // "\x1b[3" | "1mhello".
        let mut s = AnsiStripper::new();
        let mut out = s.strip(b"\x1b[3");
        out.extend(s.strip(b"1mhello"));
        assert_eq!(out, b"hello");

        // OSC body (title) split before the BEL terminator: split as
        // "\x1b]0;tit" | "le\x07after".
        let mut s = AnsiStripper::new();
        let mut out = s.strip(b"\x1b]0;tit");
        out.extend(s.strip(b"le\x07after"));
        assert_eq!(out, b"after");

        // OSC 8 hyperlink split exactly at the ST (`ESC \`) boundary — the
        // most awkward split, since the terminator itself straddles the
        // chunk boundary: "...\x1b" | "\\link".
        let mut s = AnsiStripper::new();
        let mut out = s.strip(b"\x1b]8;;http://example/\x1b");
        out.extend(s.strip(b"\\link"));
        assert_eq!(out, b"link");

        // Bare ESC at the very end of one chunk, CSI continues in the next:
        // "\x1b" | "[1mA".
        let mut s = AnsiStripper::new();
        let mut out = s.strip(b"\x1b");
        out.extend(s.strip(b"[1mA"));
        assert_eq!(out, b"A");

        // Sanity: a single instance also strips a sequence spread over
        // three chunks, and keeps passing plain bytes through between them.
        let mut s = AnsiStripper::new();
        let mut out = s.strip(b"before\x1b[");
        out.extend(s.strip(b"38;5;"));
        out.extend(s.strip(b"196mred"));
        assert_eq!(out, b"beforered");
    }
}

#[cfg(test)]
mod conformance {
    use super::*;
    use alacritty_terminal::Term;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line, Point, Side};
    use alacritty_terminal::selection::{Selection, SelectionType};
    use alacritty_terminal::term::TermMode;
    use alacritty_terminal::term::cell::Flags;
    use alacritty_terminal::vi_mode::ViMotion;
    use alacritty_terminal::vte::ansi::Processor;

    type Rx = crossbeam_channel::Receiver<TermEvent>;

    fn harness_rx(cols: usize, rows: usize) -> (Term<EventProxy>, Processor, Rx) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let waker: Waker = std::sync::Arc::new(|| {});
        let proxy = EventProxy::new(tx, waker);
        let term = Term::new(
            TermConfig::default(),
            &TermSize {
                columns: cols,
                screen_lines: rows,
            },
            proxy,
        );
        (term, Processor::new(), rx)
    }

    fn harness(cols: usize, rows: usize) -> (Term<EventProxy>, Processor) {
        let (t, p, _rx) = harness_rx(cols, rows);
        (t, p)
    }

    fn history_harness(cols: usize, rows: usize, history: usize) -> (Term<EventProxy>, Processor) {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let waker: Waker = std::sync::Arc::new(|| {});
        let proxy = EventProxy::new(tx, waker);
        let config = TermConfig {
            scrolling_history: history,
            ..TermConfig::default()
        };
        let term = Term::new(
            config,
            &TermSize {
                columns: cols,
                screen_lines: rows,
            },
            proxy,
        );
        (term, Processor::new())
    }

    fn kitty_keyboard_harness(cols: usize, rows: usize) -> (Term<EventProxy>, Processor, Rx) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let waker: Waker = std::sync::Arc::new(|| {});
        let proxy = EventProxy::new(tx, waker);
        let config = TermConfig {
            kitty_keyboard: true,
            ..TermConfig::default()
        };
        let term = Term::new(
            config,
            &TermSize {
                columns: cols,
                screen_lines: rows,
            },
            proxy,
        );
        (term, Processor::new(), rx)
    }

    /// Concatenate everything the terminal wrote back to the PTY.
    fn drain_pty(rx: &Rx) -> String {
        let mut out = String::new();
        while let Ok(ev) = rx.try_recv() {
            if let TermEvent::PtyWrite(s) = ev {
                out.push_str(&s);
            }
        }
        out
    }

    fn feed(term: &mut Term<EventProxy>, p: &mut Processor, bytes: &[u8]) {
        p.advance(term, bytes);
    }

    /// Feed bytes through the SAME two-stage path the PTY reader
    /// thread uses — `Extractor::feed` then each `Chunk::Pass` →
    /// `Processor::advance` — so a test exercises kettle's REAL pipeline (the
    /// Extractor sits in front of the engine at runtime) instead of driving the
    /// alacritty `Processor` in isolation.
    fn feed_ex(term: &mut Term<EventProxy>, p: &mut Processor, ex: &mut Extractor, bytes: &[u8]) {
        for chunk in ex.feed(bytes) {
            if let Chunk::Pass(b) = chunk {
                p.advance(term, &b);
            }
        }
    }

    fn row_text(term: &Term<EventProxy>, row: i32) -> String {
        let g = term.grid();
        (0..g.columns())
            .map(|c| g[Point::new(Line(row), Column(c))].c)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    #[test]
    fn text_newline_and_cursor_addressing() {
        let (mut t, mut p) = harness(20, 5);
        feed(&mut t, &mut p, b"hello\r\nworld");
        assert_eq!(row_text(&t, 0), "hello");
        assert_eq!(row_text(&t, 1), "world");
        // CUP: ESC[3;2H then write — 1-based row/col.
        feed(&mut t, &mut p, b"\x1b[3;2HX");
        assert_eq!(row_text(&t, 2), " X");
    }

    /// Kettle's vi UI is deliberately backed by alacritty_terminal's native
    /// grid coordinates. Pin the property that motivated that integration:
    /// output rotation moves the cursor and visual selection with their text,
    /// then clears the selection instead of silently aliasing it to unrelated
    /// content once the bounded history evicts the selected row.
    #[test]
    fn vi_cursor_and_selection_follow_scrollback_then_clear_on_eviction() {
        let (mut term, mut processor) = history_harness(24, 3, 4);
        for index in 0..7 {
            feed(
                &mut term,
                &mut processor,
                format!("stable-row-{index:02}\r\n").as_bytes(),
            );
        }
        assert_eq!(term.grid().history_size(), 4);

        term.toggle_vi_mode();
        let start = Point::new(Line(-2), Column(0));
        let selected_row = row_text(&term, start.line.0);
        assert!(
            selected_row.starts_with("stable-row-"),
            "fixture must select a populated history row: {selected_row:?}"
        );
        term.vi_goto_point(start);
        let mut selection = Selection::new(SelectionType::Simple, start, Side::Left);
        selection.update(start, Side::Right);
        term.selection = Some(selection);
        term.vi_motion(ViMotion::Last);
        let selected_before = term
            .selection_to_string()
            .expect("visual selection is materialized");
        assert!(selected_before.contains(&selected_row));

        // One output scroll rotates the grid coordinate while preserving the
        // selected content and keeps the vi cursor in the visible scrollback.
        feed(&mut term, &mut processor, b"one-more-row\r\n");
        let selected_after = term
            .selection_to_string()
            .expect("surviving visual selection follows output");
        assert_eq!(selected_after, selected_before);
        let vi_line = term.vi_mode_cursor.point.line.0;
        assert!(
            (-4..=2).contains(&vi_line),
            "vi cursor must stay inside the bounded grid, got {vi_line}"
        );

        // Advance beyond the entire history budget. The old custom UI
        // coordinates could now point at a different row; the engine instead
        // drops the selection when its anchor rotates out.
        for index in 0..8 {
            feed(
                &mut term,
                &mut processor,
                format!("replacement-{index:02}\r\n").as_bytes(),
            );
        }
        assert!(
            term.selection.is_none(),
            "evicted vi selection must be cleared instead of aliasing replacement text"
        );
        let top = -(term.grid().history_size() as i32);
        let vi_line = term.vi_mode_cursor.point.line.0;
        assert!(
            (top..term.grid().screen_lines() as i32).contains(&vi_line),
            "vi cursor must remain in the resized/rotated grid: {vi_line}, top={top}"
        );
    }

    #[test]
    fn vi_cursor_stays_bounded_and_selection_is_invalidated_by_reflow() {
        let (mut term, mut processor) = history_harness(24, 4, 12);
        for index in 0..10 {
            feed(
                &mut term,
                &mut processor,
                format!("reflow-row-{index:02}\r\n").as_bytes(),
            );
        }

        term.toggle_vi_mode();
        let point = Point::new(Line(-3), Column(2));
        term.vi_goto_point(point);
        let mut selection = Selection::new(SelectionType::Simple, point, Side::Left);
        selection.update(point, Side::Right);
        term.selection = Some(selection);
        term.vi_motion(ViMotion::WordRight);
        assert!(term.selection.is_some());

        term.resize(TermSize {
            columns: 13,
            screen_lines: 6,
        });
        assert!(
            term.selection.is_none(),
            "column reflow must invalidate a selection whose endpoints changed shape"
        );
        let top = -(term.grid().history_size() as i32);
        let vi_point = term.vi_mode_cursor.point;
        assert!(
            (top..term.grid().screen_lines() as i32).contains(&vi_point.line.0)
                && vi_point.column.0 < term.grid().columns(),
            "native vi cursor must be clamped after reflow: {vi_point:?}"
        );
    }

    #[test]
    fn kitty_keyboard_stack_caps_and_evicts_its_oldest_mode() {
        let (mut term, mut processor, rx) = kitty_keyboard_harness(20, 5);

        // The engine's maximum keyboard stack depth is 16. A seventeenth push
        // used to remove index zero from the unrelated title stack, panicking
        // when no title had ever been saved. Distinct modes also let us prove
        // that the oldest keyboard entry, not the newest, was evicted.
        for flags in 0..=16 {
            feed(
                &mut term,
                &mut processor,
                format!("\x1b[>{flags}u").as_bytes(),
            );
        }
        feed(&mut term, &mut processor, b"\x1b[<15u\x1b[?u");

        assert_eq!(drain_pty(&rx), "\x1b[?1u");
        assert!(term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
        assert!(!term.mode().contains(TermMode::REPORT_EVENT_TYPES));
    }

    #[test]
    fn kitty_keyboard_negotiation_applies_flags_and_is_screen_local() {
        let (mut term, mut processor, rx) = kitty_keyboard_harness(20, 5);

        // Query starts at zero. Replace with disambiguation, union event types,
        // then remove disambiguation using the protocol's 1/2/3 apply modes.
        feed(
            &mut term,
            &mut processor,
            b"\x1b[?u\x1b[=1;1u\x1b[=2;2u\x1b[=1;3u\x1b[?u",
        );
        assert_eq!(drain_pty(&rx), "\x1b[?0u\x1b[?2u");
        assert!(!term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
        assert!(term.mode().contains(TermMode::REPORT_EVENT_TYPES));

        // Push flag 4, then mutate the active stack entry with union mode.
        // Queries must report the resulting flag set rather than stale stack
        // state.
        feed(&mut term, &mut processor, b"\x1b[>4u\x1b[=1;2u\x1b[?u");
        assert_eq!(drain_pty(&rx), "\x1b[?5u");

        // Main and alternate screens have independent keyboard modes and
        // stacks. Main retains flags 1|4 while alternate starts empty,
        // receives flag 8, and is discarded when DECRST 1049 returns to main.
        feed(
            &mut term,
            &mut processor,
            b"\x1b[?1049h\x1b[?u\x1b[>8u\x1b[?u\x1b[?1049l\x1b[?u",
        );
        assert_eq!(drain_pty(&rx), "\x1b[?0u\x1b[?8u\x1b[?5u");
        assert!(term.mode().contains(TermMode::DISAMBIGUATE_ESC_CODES));
        assert!(term.mode().contains(TermMode::REPORT_ALTERNATE_KEYS));
        assert!(!term.mode().contains(TermMode::REPORT_ALL_KEYS_AS_ESC));

        // Popping the final entry resets all flags, as required by the
        // protocol, even if a direct mode was active before the first push.
        feed(&mut term, &mut processor, b"\x1b[<u\x1b[?u");
        assert_eq!(drain_pty(&rx), "\x1b[?0u");
        assert!(!term.mode().intersects(TermMode::KITTY_KEYBOARD_PROTOCOL));
    }

    /// R1: a selection made while scrolled back must read the
    /// VISIBLE (history) row, not the active-screen row at the same viewport
    /// index. This guards alacritty's `Selection` coordinate contract — it
    /// expects GRID-ABSOLUTE points (viewport − display_offset, via
    /// `viewport_to_point`). kettle-ui previously stored the raw viewport line,
    /// so copying while scrolled returned the wrong/empty text. The two branches
    /// below show the bug (raw viewport) vs the fix (`viewport_to_point`) select
    /// different rows — exactly why the conversion is required.
    #[test]
    fn selection_while_scrolled_reads_visible_row_not_active_screen() {
        use alacritty_terminal::grid::Scroll;
        use alacritty_terminal::index::Side;
        use alacritty_terminal::selection::{Selection, SelectionType};
        use alacritty_terminal::term::viewport_to_point;
        // 4 visible rows; feed 8 lines so the first 4 spill into scrollback.
        let (mut t, mut p) = harness(20, 4);
        feed(
            &mut t,
            &mut p,
            b"L0\r\nL1\r\nL2\r\nL3\r\nL4\r\nL5\r\nL6\r\nL7",
        );
        // Bottom: visible rows are L4..L7. Scroll back 3 → visible top = L1.
        t.scroll_display(Scroll::Delta(3));
        let off = t.grid().display_offset();
        assert_eq!(off, 3, "scrolled back 3 lines");

        // FIXED: convert viewport row 0 (showing "L1") to its grid-absolute line.
        let a = viewport_to_point(off, Point::new(0usize, Column(0)));
        let mut s = Selection::new(SelectionType::Lines, a, Side::Left);
        s.update(a, Side::Right);
        t.selection = Some(s);
        let fixed = t.selection_to_string().unwrap_or_default();
        assert!(
            fixed.contains("L1"),
            "fixed reads the visible row: {fixed:?}"
        );
        assert!(
            !fixed.contains("L4"),
            "fixed must not read the active screen: {fixed:?}"
        );

        // BUGGY: the raw viewport line used as absolute reads the active-screen
        // row "L4" instead — the regression this conversion prevents.
        let b = Point::new(Line(0), Column(0));
        let mut s2 = Selection::new(SelectionType::Lines, b, Side::Left);
        s2.update(b, Side::Right);
        t.selection = Some(s2);
        let buggy = t.selection_to_string().unwrap_or_default();
        assert!(buggy.contains("L4"), "buggy reads active screen: {buggy:?}");
        assert_ne!(
            fixed.trim(),
            buggy.trim(),
            "display_offset conversion must change which row is copied"
        );
    }

    /// The pointer's sub-cell `Side` (which half of a cell the cursor is in) must
    /// change which boundary cells a Simple drag includes. kettle's `px_to_cell`
    /// now computes this side instead of hardcoding Left/Right; this pins the
    /// alacritty `Selection::to_range` (`range_simple`) contract kettle relies on,
    /// so a future alacritty bump that changed the trimming would fail loudly here
    /// rather than silently re-introducing the "off by one letter" selection.
    #[test]
    fn selection_side_trims_inclusive_range_per_alacritty() {
        use alacritty_terminal::index::Side;
        use alacritty_terminal::selection::{Selection, SelectionType};
        let (mut t, mut p) = harness(20, 2);
        feed(&mut t, &mut p, b"ABCDEFGH");
        let a = Point::new(Line(0), Column(2)); // 'C'
        let b = Point::new(Line(0), Column(5)); // 'F'

        // Anchor on the LEFT of 'C', end on the RIGHT of 'F' → the full inclusive
        // span C..F is copied.
        let mut wide = Selection::new(SelectionType::Simple, a, Side::Left);
        wide.update(b, Side::Right);
        t.selection = Some(wide);
        let wide_s = t.selection_to_string().unwrap_or_default();
        assert_eq!(
            wide_s, "CDEF",
            "Left-anchor/Right-end selects the full span"
        );

        // Anchor on the RIGHT of 'C', end on the LEFT of 'F' → alacritty trims BOTH
        // boundary cells (C..F → D..E). This is exactly the cell-by-cell behavior a
        // drag must reproduce as the pointer crosses each cell's midpoint.
        let mut narrow = Selection::new(SelectionType::Simple, a, Side::Right);
        narrow.update(b, Side::Left);
        t.selection = Some(narrow);
        let narrow_s = t.selection_to_string().unwrap_or_default();
        assert_eq!(
            narrow_s, "DE",
            "Right-anchor/Left-end trims both boundary cells"
        );

        assert!(
            narrow_s.len() < wide_s.len(),
            "the sub-cell side must change which boundary cells are copied"
        );
    }

    /// Agent-first (A1/A2): `screen_text_of` is the single sanctioned
    /// grid scrape behind the control server's `read_screen` and `run_command`
    /// output slicing. Pin: document order (history first, then active screen),
    /// per-line right-trim, the scrollback request capped by available history,
    /// and the reported geometry/cursor.
    #[test]
    fn screen_text_returns_history_then_screen_with_geometry() {
        use crate::term::screen_text_of;
        use alacritty_terminal::grid::Scroll;
        // 4 visible rows; feed 8 lines so 4 spill into scrollback.
        let (mut t, mut p) = harness(20, 4);
        feed(
            &mut t,
            &mut p,
            b"h0\r\nh1   \r\nh2\r\nh3\r\nv0\r\nv1\r\nv2\r\nv3",
        );
        // No scrollback requested → active screen only, right-trimmed lines.
        let s = screen_text_of(&t, 0);
        assert_eq!((s.cols, s.rows), (20, 4));
        assert_eq!(s.history_size, 4);
        assert_eq!(s.display_offset, 0, "not scrolled");
        assert_eq!(s.text, "v0\nv1\nv2\nv3\n");
        // Cursor sits after "v3" on the last active row.
        assert_eq!(s.cursor, (3, 2));

        // Request more history than exists → clamped to the 4 available, with
        // history in document order BEFORE the active screen, trailing spaces
        // trimmed ("h1   " → "h1").
        let s = screen_text_of(&t, 999);
        assert_eq!(s.text, "h0\nh1\nh2\nh3\nv0\nv1\nv2\nv3\n");

        // A partial request returns only the NEWEST history tail.
        let s = screen_text_of(&t, 2);
        assert_eq!(s.text, "h2\nh3\nv0\nv1\nv2\nv3\n");

        // Default read_screen follows the visible viewport when the user or an
        // agent scrolls back. This is distinct from explicit scrollback capture
        // above, which remains active-screen based for run_command slicing.
        t.scroll_display(Scroll::Delta(3));
        let s = screen_text_of(&t, 0);
        assert_eq!(s.display_offset, 3, "scrolled back three lines");
        assert_eq!(s.text, "h1\nh2\nh3\nv0\n");

        let s = screen_text_of(&t, 2);
        assert_eq!(
            s.text, "h2\nh3\nv0\nv1\nv2\nv3\n",
            "explicit scrollback capture keeps history + active screen semantics"
        );
    }

    /// User-reported on native Ubuntu: hyperlink/URL detection
    /// must scan the VISIBLE viewport, not the active screen, when scrolled back.
    /// `links()` indexed `grid[Line(row)]` for `row in 0..screen_lines` — always
    /// the active (bottom) screen regardless of `display_offset` — so scrolling
    /// Claude Code up painted the active screen's link underlines over the
    /// scrolled-back history ("leftover/ghost underlines from another scroll
    /// position"). The fix reads `Line(row - display_offset)`, matching the
    /// decoration/selection `display_offset` conversion. This is the sibling that fix
    /// missed.
    #[test]
    fn links_while_scrolled_read_visible_viewport_not_active_screen() {
        use alacritty_terminal::grid::Scroll;
        // 4 visible rows; feed 8 lines so 4 spill into scrollback. A URL sits in
        // history (L1) and a DIFFERENT URL on the active screen (L5).
        let (mut t, mut p) = harness(40, 4);
        feed(
            &mut t,
            &mut p,
            b"top\r\nx http://hist.test/1 y\r\nl2\r\nl3\r\nl4\r\nz http://active.test/2 w\r\nl6\r\nl7",
        );
        // Bottom (offset 0): visible rows are l4 / active / l6 / l7.
        let bottom = crate::links::links(&t);
        assert!(
            bottom.iter().any(|k| k.uri.contains("active.test")),
            "at the bottom the active-screen URL is the visible one: {bottom:?}"
        );

        // Scroll back 3 → visible top = L1 ("x http://hist.test/1 y").
        t.scroll_display(Scroll::Delta(3));
        assert_eq!(t.grid().display_offset(), 3, "scrolled back 3 lines");
        let scrolled = crate::links::links(&t);
        // The VISIBLE history link is found, at viewport row 0...
        assert!(
            scrolled
                .iter()
                .any(|k| k.uri.contains("hist.test") && k.row == 0),
            "visible history link must be detected at viewport row 0: {scrolled:?}"
        );
        // ...and the now-offscreen active-screen link is NOT reported (the ghost
        // underline the user saw). Pre-fix this failed both ways: `links()` read
        // the active screen and returned the active.test URL, never hist.test.
        assert!(
            !scrolled.iter().any(|k| k.uri.contains("active.test")),
            "offscreen active-screen URL must not be underlined over history: {scrolled:?}"
        );
    }

    /// Agent CLIs and editors print project-relative file locations constantly
    /// (`crates/foo/src/lib.rs:12:3`). Kettle should make those clickable using
    /// the pane cwd while still letting URL detection own URL-shaped text.
    #[test]
    fn links_with_cwd_detects_file_paths_without_splitting_urls() {
        let (mut t, mut p) = harness(100, 3);
        feed(
            &mut t,
            &mut p,
            b"err crates/kettle-core/src/links.rs:12:3 and https://example.test/a/b\r\nabs /etc/hosts:4",
        );

        let links = crate::links::links_with_cwd(&t, Some("/home/me/kettle"));
        assert!(
            links
                .iter()
                .any(|k| k.uri == "file:///home/me/kettle/crates/kettle-core/src/links.rs"),
            "project-relative file path should resolve against pane cwd: {links:?}"
        );
        assert!(
            links.iter().any(|k| k.uri == "file:///etc/hosts"),
            "absolute file path should become a local file URI: {links:?}"
        );
        let web_links: Vec<_> = links
            .iter()
            .filter(|k| k.uri.contains("example.test"))
            .collect();
        assert_eq!(
            web_links.len(),
            1,
            "path detection must not split a URL into an extra file link: {links:?}"
        );
        assert_eq!(web_links[0].uri, "https://example.test/a/b");
    }

    /// R1: a real drag-select (Simple selection spanning rows) made
    /// while scrolled to the top of history must copy the VISIBLE history rows,
    /// not the active screen — the exact action a user does when copying an
    /// earlier chunk of a long Claude Code / Codex conversation.
    #[test]
    fn simple_drag_selection_while_scrolled_copies_visible_rows() {
        use alacritty_terminal::grid::Scroll;
        use alacritty_terminal::index::Side;
        use alacritty_terminal::selection::{Selection, SelectionType};
        use alacritty_terminal::term::viewport_to_point;
        let (mut t, mut p) = harness(12, 3);
        feed(
            &mut t,
            &mut p,
            b"row-A\r\nrow-B\r\nrow-C\r\nrow-D\r\nrow-E\r\nrow-F",
        );
        // active screen = row-D/E/F; history = row-A/B/C. Scroll to the top.
        t.scroll_display(Scroll::Delta(3));
        let off = t.grid().display_offset();
        assert_eq!(off, 3, "scrolled to the top of a 3-line history");
        // Drag from viewport (0,0) down to (1, end): copies "row-A" + "row-B".
        let start = viewport_to_point(off, Point::new(0usize, Column(0)));
        let end = viewport_to_point(off, Point::new(1usize, Column(11)));
        let mut s = Selection::new(SelectionType::Simple, start, Side::Left);
        s.update(end, Side::Right);
        t.selection = Some(s);
        let copied = t.selection_to_string().unwrap_or_default();
        assert!(
            copied.contains("row-A"),
            "copied the visible top rows: {copied:?}"
        );
        assert!(copied.contains("row-B"), "{copied:?}");
        assert!(
            !copied.contains("row-D"),
            "must not read the active screen while scrolled: {copied:?}"
        );
    }

    /// E2e harness: replay an asciicast v2 trace — the format
    /// `record.rs` writes — through the REAL VT pipeline and assert the grid
    /// reflects it. This is the `.cast` record→replay regression path: a captured
    /// Claude Code / Codex / tmux session can be re-fed deterministically (no
    /// PTY, no auth) to guard rendering, selection, and SGR handling.
    #[test]
    fn replays_asciicast_v2_output_into_grid() {
        // A minimal hand-authored trace (no real session data): plain text, an
        // SGR-bold run, and a CRLF — the shapes a Claude Code frame emits.
        let cast = concat!(
            "{\"version\":2,\"width\":20,\"height\":4}\n",
            "[0.10, \"o\", \"hello \"]\n",
            "[0.20, \"o\", \"\\u001b[1mworld\\u001b[0m\"]\n",
            "[0.30, \"o\", \"\\r\\nsecond line\"]\n",
        );
        let (mut t, mut p) = harness(20, 4);
        for line in cast.lines().skip(1) {
            let v: serde_json::Value = serde_json::from_str(line).expect("event is valid JSON");
            if v[1] == "o" {
                feed(&mut t, &mut p, v[2].as_str().unwrap_or("").as_bytes());
            }
        }
        assert_eq!(row_text(&t, 0), "hello world");
        assert_eq!(row_text(&t, 1), "second line");
        // The SGR bold from the trace applied to "world".
        let g = t.grid();
        assert!(
            g[Point::new(Line(0), Column(6))]
                .flags
                .contains(Flags::BOLD),
            "replayed SGR bold must reach the grid"
        );
    }

    /// Agent-first (A1): the full record→replay round trip through the
    /// PROMOTED `kettle_core::record::Recorder` — the same recorder that backs
    /// `kettle exec --record` and the GUI's `--record`. Record output via the
    /// real Recorder to a `.cast`, parse it back, replay through the grid, and
    /// assert it reconstructs. Pins that the recorder's on-disk format stays
    /// replayable (sibling of `replays_asciicast_v2_output_into_grid`).
    #[test]
    fn recorder_output_round_trips_through_replay() {
        use crate::record::Recorder;
        use std::io::Read;
        let temp = crate::record::test_tempdir();
        let path = temp.path().join("round-trip.cast");
        {
            let mut rec = Recorder::start(&path, 20, 4, false).expect("start recorder");
            rec.record_output(b"hello ");
            rec.record_output(b"\x1b[1mworld\x1b[0m");
            rec.record_output(b"\r\nsecond line");
            rec.finish();
        }
        let mut cast = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut cast)
            .unwrap();
        let (mut t, mut p) = harness(20, 4);
        for line in cast.lines().skip(1) {
            let v: serde_json::Value = serde_json::from_str(line).expect("event is valid JSON");
            if v[1] == "o" {
                feed(&mut t, &mut p, v[2].as_str().unwrap_or("").as_bytes());
            }
        }
        assert_eq!(row_text(&t, 0), "hello world");
        assert_eq!(row_text(&t, 1), "second line");
        assert!(
            t.grid()[Point::new(Line(0), Column(6))]
                .flags
                .contains(Flags::BOLD),
            "recorded+replayed SGR bold must reach the grid"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// R1 render completion: locks the contract kettle-render relies
    /// on to position per-cell bg / underline / strikeout / selection quads. The
    /// `display_iter` yields GRID-ABSOLUTE lines (negative when scrolled into
    /// history), and `viewport_row = grid_line + display_offset` recovers the
    /// 0-based viewport row. The render bug used the raw grid-absolute line as the
    /// viewport Y, so decorations detached from text (and a scrolled-back
    /// selection's highlight was dropped) while scrolled.
    #[test]
    fn display_iter_is_grid_absolute_so_render_adds_display_offset() {
        use alacritty_terminal::grid::Scroll;
        let (mut t, mut p) = harness(12, 4);
        feed(
            &mut t,
            &mut p,
            b"A0\r\nA1\r\nA2\r\nA3\r\nA4\r\nA5\r\nA6\r\nA7",
        );
        t.scroll_display(Scroll::Delta(3));
        let off = t.grid().display_offset() as i32;
        assert_eq!(off, 3, "scrolled back 3");
        let first = t
            .grid()
            .display_iter()
            .next()
            .expect("at least one visible cell");
        // The top visible cell is grid-absolute line -off (in history), and
        // grid_line + display_offset == 0 == the pane-top viewport row.
        assert_eq!(
            first.point.line.0, -off,
            "display_iter is grid-absolute (negative in history)"
        );
        assert_eq!(
            first.point.line.0 + off,
            0,
            "grid_line + display_offset = viewport row 0 (what kettle-render must use)"
        );
    }

    #[test]
    fn erase_line_and_display() {
        let (mut t, mut p) = harness(10, 3);
        feed(&mut t, &mut p, b"ABCDEFG");
        feed(&mut t, &mut p, b"\x1b[1;4H\x1b[K"); // cursor col4, erase to EOL
        assert_eq!(row_text(&t, 0), "ABC");
        feed(&mut t, &mut p, b"\x1b[2J"); // erase whole display
        assert_eq!(row_text(&t, 0), "");
    }

    #[test]
    fn sgr_truecolor_bold_and_reset() {
        use alacritty_terminal::vte::ansi::Color;
        let (mut t, mut p) = harness(8, 2);
        feed(&mut t, &mut p, b"\x1b[1;38;2;10;20;30mZ\x1b[0mz");
        let g = t.grid();
        let z = &g[Point::new(Line(0), Column(0))];
        assert!(z.flags.contains(Flags::BOLD));
        match z.fg {
            Color::Spec(rgb) => assert_eq!((rgb.r, rgb.g, rgb.b), (10, 20, 30)),
            other => panic!("expected truecolor, got {other:?}"),
        }
        // After SGR reset, the next cell is back to default fg + no bold.
        let z2 = &g[Point::new(Line(0), Column(1))];
        assert!(!z2.flags.contains(Flags::BOLD));
    }

    #[test]
    fn tab_stops_and_carriage_return() {
        let (mut t, mut p) = harness(20, 2);
        feed(&mut t, &mut p, b"a\tb");
        let s = row_text(&t, 0);
        assert_eq!(&s[..1], "a");
        assert_eq!(s.chars().nth(8), Some('b')); // default tab stop at col 8
        feed(&mut t, &mut p, b"\rZ");
        assert_eq!(row_text(&t, 0).chars().next(), Some('Z'));
    }

    #[test]
    fn alt_screen_and_bracketed_paste_modes() {
        let (mut t, mut p) = harness(10, 3);
        feed(&mut t, &mut p, b"\x1b[?1049h");
        assert!(t.mode().contains(TermMode::ALT_SCREEN));
        feed(&mut t, &mut p, b"\x1b[?2004h");
        assert!(t.mode().contains(TermMode::BRACKETED_PASTE));
        feed(&mut t, &mut p, b"\x1b[?1049l");
        assert!(!t.mode().contains(TermMode::ALT_SCREEN));
    }

    #[test]
    fn scroll_region_and_index() {
        let (mut t, mut p) = harness(6, 4);
        // Restrict scrolling to rows 1..=2 (DECSTBM, 1-based), then make it
        // scroll: cursor to row2, newlines push row1 content up within region.
        feed(
            &mut t,
            &mut p,
            b"\x1b[1;2r\x1b[1;1Hone\x1b[2;1Htwo\r\n\r\nthree",
        );
        // Row 3 (outside the region) stays empty; region scrolled.
        assert_eq!(row_text(&t, 3), "");
    }

    #[test]
    fn dec_special_graphics_charset() {
        let (mut t, mut p) = harness(6, 2);
        // ESC ( 0 = DEC line-drawing into G0; q->─ x->│; ESC ( B back to ASCII.
        feed(&mut t, &mut p, b"\x1b(0qx\x1b(By");
        let s = row_text(&t, 0);
        let cs: Vec<char> = s.chars().collect();
        assert_eq!(cs[0], '\u{2500}', "q -> light horizontal");
        assert_eq!(cs[1], '\u{2502}', "x -> light vertical");
        assert_eq!(cs[2], 'y', "ASCII restored after ESC(B");
    }

    #[test]
    fn insert_and_delete_char() {
        let (mut t, mut p) = harness(8, 2);
        feed(&mut t, &mut p, b"abcde");
        // Cursor to col 2 ('b'), delete it (DCH).
        feed(&mut t, &mut p, b"\x1b[1;2H\x1b[P");
        assert_eq!(row_text(&t, 0), "acde");
        // Insert a blank at col 1 (ICH), then type at it.
        feed(&mut t, &mut p, b"\x1b[1;1H\x1b[@Z");
        assert_eq!(row_text(&t, 0), "Zacde");
    }

    #[test]
    fn insert_and_delete_line() {
        let (mut t, mut p) = harness(6, 4);
        feed(&mut t, &mut p, b"r0\r\nr1\r\nr2");
        // Delete line 2 (DL): r2 moves up into row 1.
        feed(&mut t, &mut p, b"\x1b[2;1H\x1b[M");
        assert_eq!(row_text(&t, 0), "r0");
        assert_eq!(row_text(&t, 1), "r2");
        // Insert a line at row 1 (IL): pushes r2 back down.
        feed(&mut t, &mut p, b"\x1b[2;1H\x1b[L");
        assert_eq!(row_text(&t, 1), "");
        assert_eq!(row_text(&t, 2), "r2");
    }

    #[test]
    fn save_restore_cursor_and_autowrap() {
        let (mut t, mut p) = harness(3, 3);
        // DECSC at row1col1, move away, write, DECRC back, overwrite.
        feed(&mut t, &mut p, b"\x1b7\x1b[3;3HX\x1b8A");
        assert_eq!(row_text(&t, 0).chars().next(), Some('A'));
        // Autowrap (DECAWM, on by default): 4 chars into 3 columns wraps.
        let (mut t2, mut p2) = harness(3, 3);
        feed(&mut t2, &mut p2, b"abcd");
        assert_eq!(row_text(&t2, 0), "abc");
        assert_eq!(row_text(&t2, 1), "d");
    }

    #[test]
    fn origin_mode_addresses_within_margins() {
        let (mut t, mut p) = harness(6, 5);
        // Scroll region rows 2..=4, enable origin mode, then home (1;1) is
        // the region top (absolute row index 1).
        feed(&mut t, &mut p, b"\x1b[2;4r\x1b[?6h\x1b[1;1HO");
        assert_eq!(row_text(&t, 0), "", "row 0 is above the margin");
        assert_eq!(row_text(&t, 1), "O", "origin-mode home = top margin");
    }

    #[test]
    fn dsr_cursor_position_report() {
        let (mut t, mut p, rx) = harness_rx(40, 10);
        // Move to row 3, col 5 (1-based), then DSR 6n.
        feed(&mut t, &mut p, b"\x1b[3;5H\x1b[6n");
        let reply = drain_pty(&rx);
        assert_eq!(reply, "\x1b[3;5R", "CPR must echo the 1-based cursor");
    }

    #[test]
    fn device_attributes_reply() {
        let (mut t, mut p, rx) = harness_rx(10, 3);
        feed(&mut t, &mut p, b"\x1b[c"); // Primary DA
        let reply = drain_pty(&rx);
        assert!(
            reply.starts_with("\x1b[?"),
            "DA1 reply should be a CSI ? … c, got {reply:?}"
        );
        assert!(reply.ends_with('c'));
    }

    #[test]
    fn sgr_underline_dim_strike() {
        let (mut t, mut p) = harness(8, 2);
        // dim + single underline + strikeout, then a curly underline cell.
        feed(&mut t, &mut p, b"\x1b[2;4;9mA\x1b[0m\x1b[4:3mB");
        let g = t.grid();
        let a = &g[Point::new(Line(0), Column(0))];
        assert!(a.flags.contains(Flags::DIM));
        assert!(a.flags.contains(Flags::UNDERLINE));
        assert!(a.flags.contains(Flags::STRIKEOUT));
        let b = &g[Point::new(Line(0), Column(1))];
        assert!(
            b.flags.contains(Flags::UNDERCURL),
            "SGR 4:3 = curly underline"
        );
        assert!(!b.flags.contains(Flags::DIM), "SGR 0 reset cleared dim");
    }

    #[test]
    fn decaln_fills_screen_with_e() {
        let (mut t, mut p) = harness(4, 3);
        feed(&mut t, &mut p, b"\x1b#8"); // DEC screen alignment test
        for r in 0..3 {
            assert_eq!(row_text(&t, r), "EEEE", "DECALN fills row {r}");
        }
    }

    #[test]
    fn rep_repeats_last_graphic_char() {
        let (mut t, mut p) = harness(8, 2);
        // 'A' then REP 3 -> "AAAA".
        feed(&mut t, &mut p, b"A\x1b[3b");
        assert_eq!(row_text(&t, 0), "AAAA");
    }

    #[test]
    fn charset_g1_via_so_si() {
        let (mut t, mut p) = harness(6, 2);
        // Designate DEC special graphics into G1, SO -> G1, SI -> back to G0.
        feed(&mut t, &mut p, b"\x1b)0\x0eqx\x0fy");
        let cs: Vec<char> = row_text(&t, 0).chars().collect();
        assert_eq!(cs[0], '\u{2500}', "G1 q -> horizontal line");
        assert_eq!(cs[1], '\u{2502}', "G1 x -> vertical line");
        assert_eq!(cs[2], 'y', "SI returned to ASCII G0");
    }

    #[test]
    fn ris_full_reset_clears_origin_mode() {
        let (mut t, mut p) = harness(6, 4);
        // Origin mode on + scroll region, then RIS (ESC c) — a full reset —
        // so 1;1 is absolute home again.
        feed(&mut t, &mut p, b"\x1b[2;4r\x1b[?6hzz\x1bc\x1b[1;1HX");
        assert_eq!(row_text(&t, 0), "X", "RIS cleared origin mode + region");
    }

    #[test]
    fn el_erase_to_left() {
        let (mut t, mut p) = harness(6, 2);
        feed(&mut t, &mut p, b"ABCDE");
        // Cursor to col 3 (1-based), EL 1 = erase start..=cursor.
        feed(&mut t, &mut p, b"\x1b[1;3H\x1b[1K");
        // cols 0..=2 cleared; "DE" remains at cols 3,4.
        assert_eq!(row_text(&t, 0), "   DE");
    }

    #[test]
    fn ed_erase_below() {
        let (mut t, mut p) = harness(4, 3);
        feed(&mut t, &mut p, b"r0\r\nr1\r\nr2");
        // Cursor to row 2 col 1 (1-based), ED 0 = erase cursor..=end.
        feed(&mut t, &mut p, b"\x1b[2;1H\x1b[0J");
        assert_eq!(row_text(&t, 0), "r0", "row above the cursor kept");
        assert_eq!(row_text(&t, 1), "", "cursor row erased");
        assert_eq!(row_text(&t, 2), "", "rows below erased");
    }

    #[test]
    fn da2_secondary_device_attributes() {
        let (mut t, mut p, rx) = harness_rx(10, 3);
        feed(&mut t, &mut p, b"\x1b[>c"); // Secondary DA
        let reply = drain_pty(&rx);
        assert!(
            reply.starts_with("\x1b[>") && reply.ends_with('c'),
            "DA2 reply should be CSI > … c, got {reply:?}"
        );
    }

    #[test]
    fn ech_erases_in_place() {
        let (mut t, mut p) = harness(6, 2);
        feed(&mut t, &mut p, b"ABCDE");
        // Cursor col 2 (1-based), ECH 2 clears 2 cells, cursor unmoved.
        feed(&mut t, &mut p, b"\x1b[1;2H\x1b[2X");
        assert_eq!(row_text(&t, 0), "A  DE");
    }

    #[test]
    fn ich_shifts_right_off_edge() {
        let (mut t, mut p) = harness(5, 2);
        feed(&mut t, &mut p, b"abcde");
        // ICH 2 at home pushes cells right; the line is 5 wide so d,e fall off.
        feed(&mut t, &mut p, b"\x1b[1;1H\x1b[2@");
        assert_eq!(row_text(&t, 0), "  abc");
    }

    #[test]
    fn absolute_cursor_moves_cha_hpa_vpa() {
        let (mut t, mut p) = harness(6, 4);
        // CHA: column-absolute to col 3 then write.
        feed(&mut t, &mut p, b"abcde\x1b[3GZ");
        assert_eq!(row_text(&t, 0), "abZde");
        // HPA (ESC[`) col 2 on row 1, VPA (ESC[d) row 3.
        feed(&mut t, &mut p, b"\x1b[1;1H\x1b[2`Q\x1b[3dW");
        assert_eq!(row_text(&t, 0).chars().nth(1), Some('Q'), "HPA col 2");
        // VPA changes the row only; column stays where it was (col 3 after Q).
        assert_eq!(row_text(&t, 2).chars().nth(2), Some('W'), "VPA row 3");
    }

    #[test]
    fn decsc_restores_sgr_attributes() {
        let (mut t, mut p) = harness(6, 3);
        // Bold on, DECSC (saves cursor + pen), reset SGR + move away,
        // DECRC restores both, then write — must be bold at the saved cell.
        feed(&mut t, &mut p, b"\x1b[1m\x1b7\x1b[0m\x1b[3;4HZ\x1b8A");
        let a = &t.grid()[Point::new(Line(0), Column(0))];
        assert_eq!(a.c, 'A');
        assert!(
            a.flags.contains(Flags::BOLD),
            "DECRC must restore the saved SGR pen"
        );
    }

    /// Synchronized output (DEC private mode 2026 / BSU·ESU). While a
    /// sync block is open the engine MUST buffer mutations so a renderer that
    /// locks the grid never samples a half-drawn frame; the buffered changes
    /// apply atomically on close. This is the property that lets well-behaved
    /// TUIs avoid the transient mid-repaint tearing a terminal would otherwise
    /// show. The bytes are fed through kettle's REAL pipeline (`feed_ex` →
    /// Extractor → Processor), so this also guards that a future `Extractor`
    /// change cannot swallow the `?2026` toggles (this test previously
    /// fed bytes straight to the Processor, bypassing the Extractor it claims to
    /// guard).
    #[test]
    fn synchronized_update_defers_grid_mutation_until_close() {
        let (mut t, mut p) = harness(6, 2);
        let mut ex = Extractor::new();
        feed_ex(&mut t, &mut p, &mut ex, b"A");
        assert_eq!(t.grid()[Point::new(Line(0), Column(0))].c, 'A');
        // Open a synchronized update, return to col 0 and overwrite with 'B',
        // but DO NOT close the block yet.
        feed_ex(&mut t, &mut p, &mut ex, b"\x1b[?2026h\rB");
        assert_eq!(
            t.grid()[Point::new(Line(0), Column(0))].c,
            'A',
            "grid mutated mid-synchronized-update (mode 2026 not honored)"
        );
        // Close the block — the buffered write now applies atomically.
        feed_ex(&mut t, &mut p, &mut ex, b"\x1b[?2026l");
        assert_eq!(
            t.grid()[Point::new(Line(0), Column(0))].c,
            'B',
            "synchronized update not flushed on close"
        );
    }

    #[test]
    fn su_sd_scroll_up_and_down() {
        let (mut t, mut p) = harness(4, 3);
        feed(&mut t, &mut p, b"r0\r\nr1\r\nr2");
        feed(&mut t, &mut p, b"\x1b[1S"); // SU 1: content moves up
        assert_eq!(row_text(&t, 0), "r1");
        assert_eq!(row_text(&t, 1), "r2");
        assert_eq!(row_text(&t, 2), "");

        let (mut t2, mut p2) = harness(4, 3);
        feed(&mut t2, &mut p2, b"a\r\nb\r\nc");
        feed(&mut t2, &mut p2, b"\x1b[1T"); // SD 1: content moves down
        assert_eq!(row_text(&t2, 0), "");
        assert_eq!(row_text(&t2, 1), "a");
        assert_eq!(row_text(&t2, 2), "b");
    }

    #[test]
    fn decscusr_sets_cursor_shape() {
        use alacritty_terminal::vte::ansi::CursorShape;
        let (mut t, mut p) = harness(6, 2);
        feed(&mut t, &mut p, b"\x1b[3 q"); // DECSCUSR 3 = (blinking) underline
        assert_eq!(t.renderable_content().cursor.shape, CursorShape::Underline);
        feed(&mut t, &mut p, b"\x1b[5 q"); // 5 = (blinking) bar/beam
        assert_eq!(t.renderable_content().cursor.shape, CursorShape::Beam);
        feed(&mut t, &mut p, b"\x1b[1 q"); // 1 = (blinking) block
        assert_eq!(t.renderable_content().cursor.shape, CursorShape::Block);
    }

    #[test]
    fn dec_mode_25_hide_collapses_renderable_cursor_to_hidden() {
        // Cursor visibility (DEC ?25) and cursor shape (DECSCUSR `q`) are
        // tracked in different places in the engine; `RenderableContent`
        // *folds* them so the renderer only has to look at one field. This
        // test pins that contract — what the renderer reads is `Hidden` the
        // moment a program clears ?25, even if the shape was set to
        // something else first. Otherwise we'd silently keep drawing a
        // cursor at TUI apps that asked us not to (less, fzf full-screen).
        use alacritty_terminal::vte::ansi::CursorShape;
        let (mut t, mut p) = harness(6, 2);
        feed(&mut t, &mut p, b"\x1b[1 q"); // shape = block (visible default)
        assert_eq!(t.renderable_content().cursor.shape, CursorShape::Block);
        feed(&mut t, &mut p, b"\x1b[?25l"); // ?25 cleared = hide cursor
        assert_eq!(
            t.renderable_content().cursor.shape,
            CursorShape::Hidden,
            "DEC ?25 l must collapse the renderable cursor to Hidden"
        );
        feed(&mut t, &mut p, b"\x1b[?25h"); // ?25 set = show again
        assert_eq!(
            t.renderable_content().cursor.shape,
            CursorShape::Block,
            "DEC ?25 h restores the previous shape"
        );
    }

    #[test]
    fn wide_cjk_char_occupies_two_cells() {
        let (mut t, mut p) = harness(8, 2);
        feed(&mut t, &mut p, "世A".as_bytes());
        let g = t.grid();
        let c0 = &g[Point::new(Line(0), Column(0))];
        assert_eq!(c0.c, '世');
        assert!(c0.flags.contains(Flags::WIDE_CHAR), "CJK = wide");
        assert!(
            g[Point::new(Line(0), Column(1))]
                .flags
                .contains(Flags::WIDE_CHAR_SPACER),
            "second cell is the wide spacer"
        );
        assert_eq!(g[Point::new(Line(0), Column(2))].c, 'A');
    }

    #[test]
    fn wide_char_wraps_when_it_does_not_fit() {
        let (mut t, mut p) = harness(3, 3);
        // 2 narrow + 1 wide: the wide char can't fit in the last column,
        // so it wraps to the next row.
        feed(&mut t, &mut p, "ab世".as_bytes());
        assert_eq!(row_text(&t, 0), "ab");
        assert_eq!(t.grid()[Point::new(Line(1), Column(0))].c, '世');
    }

    #[test]
    fn combining_mark_is_zero_width() {
        let (mut t, mut p) = harness(6, 2);
        // 'e' + combining acute accent: one cell, mark stored as zerowidth.
        feed(&mut t, &mut p, "e\u{0301}X".as_bytes());
        let g = t.grid();
        let base = &g[Point::new(Line(0), Column(0))];
        assert_eq!(base.c, 'e');
        assert_eq!(
            base.zerowidth(),
            Some(&['\u{0301}'][..]),
            "combining mark attaches to the base cell"
        );
        assert_eq!(
            g[Point::new(Line(0), Column(1))].c,
            'X',
            "next glyph is in the very next cell (mark took no column)"
        );
    }

    #[test]
    fn osc4_palette_query_emits_color_request() {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        // Query palette entry 1.
        feed(&mut t, &mut p, b"\x1b]4;1;?\x07");
        let got_idx = rx.try_iter().find_map(|ev| match ev {
            TermEvent::ColorRequest(idx, _) => Some(idx),
            _ => None,
        });
        assert_eq!(got_idx, Some(1), "OSC 4 ; 1 ; ? requests palette index 1");
    }

    #[test]
    fn sgr_underline_style_variants_set_distinct_flags() {
        // Five style bits in the engine: `\e[4m` (single), `\e[21m` or
        // `\e[4:2m` (double), `\e[4:3m` (curl), `\e[4:4m` (dotted),
        // `\e[4:5m` (dashed). The renderer reads these and
        // draws differently per style — this pins each one reaching the
        // engine's cell flags so a future engine bump can't silently
        // drop a variant.
        let (mut t, mut p) = harness(20, 2);
        feed(
            &mut t,
            &mut p,
            b"\x1b[4ma\x1b[4:2mb\x1b[4:3mc\x1b[4:4md\x1b[4:5me",
        );
        let g = t.grid();
        let f = |c: usize| g[Point::new(Line(0), Column(c))].flags;
        assert!(f(0).contains(Flags::UNDERLINE), "[4m → UNDERLINE on `a`");
        assert!(
            f(1).contains(Flags::DOUBLE_UNDERLINE),
            "[4:2m → DOUBLE_UNDERLINE on `b`"
        );
        assert!(f(2).contains(Flags::UNDERCURL), "[4:3m → UNDERCURL on `c`");
        assert!(
            f(3).contains(Flags::DOTTED_UNDERLINE),
            "[4:4m → DOTTED_UNDERLINE on `d`"
        );
        assert!(
            f(4).contains(Flags::DASHED_UNDERLINE),
            "[4:5m → DASHED_UNDERLINE on `e`"
        );
        // Each variant is mutually-exclusive: setting DOUBLE clears the
        // previous UNDERLINE bit (alacritty single-underline-flag model).
        // Confirm by checking `b` doesn't still carry plain UNDERLINE.
        assert!(
            !f(1).contains(Flags::UNDERLINE),
            "[4:2m must clear plain UNDERLINE"
        );
    }

    #[test]
    fn sgr_58_sets_per_cell_underline_color() {
        // Neovim spell-check / git diff / lsp diagnostics emit per-cell
        // underline color via SGR 58. The engine stores it on the cell;
        // the renderer reads it so the squiggle color follows
        // the request instead of using the text fg. Confirms truecolor
        // form `\e[58;2;r;g;bm` reaches `cell.underline_color()`.
        use alacritty_terminal::vte::ansi::{Color as AnsiColor, Rgb as AnsiRgb};
        let (mut t, mut p) = harness(8, 2);
        // Underline on + red underline color, then write a glyph.
        feed(&mut t, &mut p, b"\x1b[4m\x1b[58;2;200;30;30mX");
        let grid = t.grid();
        let cell = &grid[Point::new(Line(0), Column(0))];
        assert_eq!(cell.c, 'X');
        assert!(
            cell.flags.contains(Flags::UNDERLINE),
            "SGR 4 must set UNDERLINE"
        );
        assert_eq!(
            cell.underline_color(),
            Some(AnsiColor::Spec(AnsiRgb {
                r: 200,
                g: 30,
                b: 30
            })),
            "SGR 58 must store the per-cell underline color"
        );
        // SGR 59 resets to default (None), leaving UNDERLINE intact.
        feed(&mut t, &mut p, b"\x1b[59mY");
        let cell2 = &t.grid()[Point::new(Line(0), Column(1))];
        assert_eq!(cell2.c, 'Y');
        assert!(cell2.flags.contains(Flags::UNDERLINE));
        assert_eq!(
            cell2.underline_color(),
            None,
            "SGR 59 must clear the per-cell underline color"
        );
    }

    #[test]
    fn osc4_multi_index_query_emits_one_request_per_index() {
        // vte's OSC 4 handler chunks the params in pairs (`;idx;val`), so
        // a single `OSC 4 ; 1 ; ? ; 7 ; ?` should ask for *two* colors in
        // one go. tmux, neovim 0.10+ and base16-shell-hook all batch
        // palette probes this way — without per-pair dispatch they'd see
        // only the first reply and assume the rest of the palette equals
        // the engine default, breaking the dark/light detection they rely
        // on.
        let (mut t, mut p, rx) = harness_rx(8, 2);
        feed(&mut t, &mut p, b"\x1b]4;1;?;7;?\x07");
        let mut indices: Vec<usize> = rx
            .try_iter()
            .filter_map(|ev| match ev {
                TermEvent::ColorRequest(idx, _) => Some(idx),
                _ => None,
            })
            .collect();
        indices.sort_unstable();
        assert_eq!(
            indices,
            vec![1, 7],
            "multi-index OSC 4 must fire one ColorRequest per `;idx;?` pair"
        );
    }

    #[test]
    fn osc_10_11_12_set_populate_default_color_slots() {
        // OSC 10/11/12 SET should populate the engine's `Colors[256..=258]`
        // slots (default fg, default bg, cursor) so the renderer's
        // `resolve_query` reflects the override on the next frame. Without
        // this round-trip, OSC 12 (set cursor color) was a silent drop in
        // the render path. Confirms the
        // pair: OSC 4 set is covered by `osc_color_set_query_reset_round_trip_through_engine`;
        // OSC 10/11/12 are the close siblings that use the same Colors slots.
        for (input, idx) in &[
            (b"\x1b]10;rgb:11/22/33\x07" as &[u8], 256usize),
            (b"\x1b]11;rgb:44/55/66\x07", 257),
            (b"\x1b]12;rgb:77/88/99\x07", 258),
        ] {
            let (mut t, mut p, _rx) = harness_rx(8, 2);
            assert!(t.colors()[*idx].is_none(), "slot {idx} clean pre-set");
            feed(&mut t, &mut p, input);
            let c = t.colors()[*idx].unwrap_or_else(|| panic!("slot {idx} unset after {input:?}"));
            // The exact values from the xparsecolor input (engine packs
            // each `RR` byte pair into a single u8).
            let want = match idx {
                256 => (0x11, 0x22, 0x33),
                257 => (0x44, 0x55, 0x66),
                258 => (0x77, 0x88, 0x99),
                _ => unreachable!(),
            };
            assert_eq!((c.r, c.g, c.b), want, "wrong color for slot {idx}");
        }
    }

    #[test]
    fn osc_104_no_params_resets_all_256_palette_slots() {
        // OSC 104 with no parameters (just `\e]104\a` or `\e]104;\a`)
        // resets *every* palette index (0..256), not just one. xterm
        // documents this: "OSC 104 ; c → reset color number c (default
        // restore palette)." Tools like `colorls`/`zsh-colorize`'s
        // theme-changers emit it to undo their session-wide palette
        // overrides on exit. The `osc_color_set_query_reset_round_trip_through_engine`
        // test covered only the indexed form (`\e]104;1\a`); pin the
        // no-arg-resets-all branch too so it can't quietly regress
        // (e.g. if alacritty/vte upstream change the dispatch table).
        let (mut t, mut p, _rx) = harness_rx(8, 2);
        // Populate three slots so we have something to confirm reset against.
        feed(&mut t, &mut p, b"\x1b]4;1;rgb:11/22/33\x07");
        feed(&mut t, &mut p, b"\x1b]4;2;rgb:44/55/66\x07");
        feed(&mut t, &mut p, b"\x1b]4;200;rgb:77/88/99\x07");
        assert!(t.colors()[1].is_some(), "slot 1 should be set");
        assert!(t.colors()[2].is_some(), "slot 2 should be set");
        assert!(t.colors()[200].is_some(), "slot 200 should be set");
        // OSC 104 with no parameters → reset all 256 palette indices.
        feed(&mut t, &mut p, b"\x1b]104\x07");
        for idx in 0..256 {
            assert!(
                t.colors()[idx].is_none(),
                "slot {idx} should be cleared after OSC 104 (no params)"
            );
        }
    }

    #[test]
    fn osc_110_111_112_reset_default_fg_bg_cursor_slots() {
        // OSC 110 / 111 / 112 are the reset siblings of OSC 10/11/12 (set
        // default fg/bg/cursor). They tell the engine to throw away any
        // override the user-program set so the renderer falls back to the
        // theme's defaults. Kettle's render path reads `t.colors()[256..=258]`
        // each frame; if the engine didn't honor these resets, a program
        // that did `\e]10;rgb:11/22/33\a` then `\e]110\a` to undo would
        // leave the (red) override in place — a real bug class where the
        // set path gets fixed but the reset path silently stays broken
        // (this test pins the reset path so it can't regress in the other
        // direction). Same loop covers all three indices in one
        // declarative table.
        for (idx, set, reset) in &[
            (
                256usize,
                &b"\x1b]10;rgb:11/22/33\x07"[..],
                &b"\x1b]110\x07"[..],
            ),
            (
                257usize,
                &b"\x1b]11;rgb:44/55/66\x07"[..],
                &b"\x1b]111\x07"[..],
            ),
            (
                258usize,
                &b"\x1b]12;rgb:77/88/99\x07"[..],
                &b"\x1b]112\x07"[..],
            ),
        ] {
            let (mut t, mut p, _rx) = harness_rx(8, 2);
            assert!(t.colors()[*idx].is_none(), "slot {idx} clean pre-set");
            feed(&mut t, &mut p, set);
            assert!(
                t.colors()[*idx].is_some(),
                "slot {idx} should be populated after OSC set"
            );
            feed(&mut t, &mut p, reset);
            assert!(
                t.colors()[*idx].is_none(),
                "slot {idx} should be cleared after the OSC reset sibling"
            );
        }
    }

    #[test]
    fn osc_color_set_query_reset_round_trip_through_engine() {
        // Round-trip companion to the OSC query test: confirm OSC 4 set +
        // OSC 104 reset actually move the engine's `Colors` slot (so our
        // `kettle_render::resolve_query` will reflect changes live —
        // tested separately in kettle-render). This guards against an
        // upstream regression silently disconnecting the set/reset path
        // from the OSC 4 query path we ship.
        let (mut t, mut p, _rx) = harness_rx(8, 2);
        // Initially the engine has no override for palette 1.
        assert!(t.colors()[1].is_none(), "expected no override pre-set");
        // OSC 4 ; 1 ; rgb:11/22/33  → set.
        feed(&mut t, &mut p, b"\x1b]4;1;rgb:11/22/33\x07");
        let after_set = t.colors()[1].expect("OSC 4 set must populate slot 1");
        assert_eq!(
            (after_set.r, after_set.g, after_set.b),
            (0x11, 0x22, 0x33),
            "engine must store the override exactly"
        );
        // OSC 104 ; 1  → reset that index only.
        feed(&mut t, &mut p, b"\x1b]104;1\x07");
        assert!(t.colors()[1].is_none(), "OSC 104 ; 1 must clear slot 1");
    }

    #[test]
    fn xtwinops_text_area_size_pixels_formats_csi4_reply() {
        // CSI 14 t — text-area pixel size. The engine raises
        // TextAreaSizeRequest(fmt) and expects the caller to plug in cell
        // dimensions + grid size; the formatter then produces the standard
        // `CSI 4 ; h ; w t` xtwinops reply (h = rows × cell_h, w = cols ×
        // cell_w). Sixel/kitty/iTerm2 image apps depend on this to compute
        // pixel-accurate placements.
        let (mut t, mut p, rx) = harness_rx(40, 10);
        feed(&mut t, &mut p, b"\x1b[14t");
        let fmt = rx
            .try_iter()
            .find_map(|ev| match ev {
                TermEvent::TextAreaSizeRequest(f) => Some(f),
                _ => None,
            })
            .expect("CSI 14 t must raise a TextAreaSizeRequest");
        // 9 px wide × 18 px tall cells on a 40×10 grid → 360 × 180 px.
        let reply = fmt(alacritty_terminal::event::WindowSize {
            num_lines: 10,
            num_cols: 40,
            cell_width: 9,
            cell_height: 18,
        });
        assert_eq!(
            reply, "\x1b[4;180;360t",
            "CSI 14 t reply must be CSI 4 ; <height-px> ; <width-px> t"
        );
    }

    #[test]
    fn osc_color_queries_carry_index_and_format_xparsecolor_reply() {
        // OSC 10 / 11 / 12 (default fg / bg / cursor) and OSC 4 ; n ; ? are
        // the four queries shells and TUIs use to detect light-vs-dark and
        // theme colors. Each must (a) emit a `ColorRequest` carrying the
        // correct index — 256 / 257 / 258 / palette-idx — and (b) hand back
        // an engine-supplied formatter that renders the canonical xparsecolor
        // reply `\e]<prefix>;rgb:RRRR/GGGG/BBBB\` so apps that probe for the
        // exact wire format (mc / neovim / gnome-terminal probes) accept it.
        let cases: &[(&[u8], usize, &str)] = &[
            (b"\x1b]10;?\x07", 256, "\x1b]10;rgb:"), // OSC 10 — fg
            (b"\x1b]11;?\x07", 257, "\x1b]11;rgb:"), // OSC 11 — bg
            (b"\x1b]12;?\x07", 258, "\x1b]12;rgb:"), // OSC 12 — cursor
            (b"\x1b]4;7;?\x07", 7, "\x1b]4;7;rgb:"), // OSC 4 ; 7 — palette
        ];
        for (input, want_idx, want_prefix) in cases {
            let (mut t, mut p, rx) = harness_rx(8, 2);
            feed(&mut t, &mut p, input);
            let (idx, fmt) = rx
                .try_iter()
                .find_map(|ev| match ev {
                    TermEvent::ColorRequest(i, f) => Some((i, f)),
                    _ => None,
                })
                .unwrap_or_else(|| panic!("no ColorRequest for {input:?}"));
            assert_eq!(idx, *want_idx, "wrong index for {input:?}");

            // Format with a known value and verify the wire shape. The
            // 8-bit channels are doubled to 16-bit per xparsecolor.
            let reply = fmt(alacritty_terminal::vte::ansi::Rgb {
                r: 0x12,
                g: 0x34,
                b: 0x56,
            });
            let want_payload = format!("{want_prefix}1212/3434/5656");
            assert!(
                reply.starts_with(&want_payload),
                "{input:?} reply must start with {want_payload:?}, got {reply:?}"
            );
        }
    }

    #[test]
    fn decrqm_reports_mode_state() {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        // Enable bracketed paste, then DECRQM-query it (CSI ? 2004 $ p).
        feed(&mut t, &mut p, b"\x1b[?2004h\x1b[?2004$p");
        let reply = drain_pty(&rx);
        assert!(
            reply.contains("2004;1") && reply.ends_with("$y"),
            "DECRPM should report mode 2004 as set, got {reply:?}"
        );
    }

    #[test]
    fn osc52_copy_emits_clipboard_store() {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        // base64("hi") = "aGk=" ; OSC 52 ; c ; <b64> ST
        feed(&mut t, &mut p, b"\x1b]52;c;aGk=\x07");
        let stored = rx.try_iter().find_map(|ev| match ev {
            TermEvent::ClipboardStore(_, s) => Some(s),
            _ => None,
        });
        assert_eq!(stored.as_deref(), Some("hi"), "OSC 52 c sets clipboard");
    }

    #[test]
    fn osc8_hyperlink_carries_on_cells() {
        let (mut t, mut p) = harness(8, 2);
        feed(
            &mut t,
            &mut p,
            b"\x1b]8;;https://x.example\x07Z\x1b]8;;\x07W",
        );
        let g = t.grid();
        let z = &g[Point::new(Line(0), Column(0))];
        assert_eq!(z.c, 'Z');
        assert_eq!(
            z.hyperlink().map(|h| h.uri().to_string()).as_deref(),
            Some("https://x.example"),
            "OSC 8 URI attaches to the cell"
        );
        // After the closing OSC 8 ; ; the link is cleared.
        assert!(g[Point::new(Line(0), Column(1))].hyperlink().is_none());
    }

    #[test]
    fn alt_screen_preserves_primary_content() {
        let (mut t, mut p) = harness(8, 3);
        feed(&mut t, &mut p, b"main");
        feed(&mut t, &mut p, b"\x1b[?1049h"); // enter alt screen
        assert_eq!(row_text(&t, 0), "", "alt screen starts blank");
        feed(&mut t, &mut p, b"\x1b[2J\x1b[1;1Halt");
        assert_eq!(row_text(&t, 0), "alt");
        feed(&mut t, &mut p, b"\x1b[?1049l"); // back to primary
        assert_eq!(row_text(&t, 0), "main", "primary content restored");
    }

    #[test]
    fn synchronized_output_applies_content() {
        let (mut t, mut p) = harness(8, 2);
        // DECSET 2026 brackets an atomic update; the content must be present
        // and correct once the synchronized update ends.
        feed(&mut t, &mut p, b"\x1b[?2026hhello\x1b[?2026l");
        assert_eq!(row_text(&t, 0), "hello");
    }

    #[test]
    fn decrqm_reports_synchronized_output_mode() {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        feed(&mut t, &mut p, b"\x1b[?2026$p"); // DECRQM query of mode 2026
        let reply = drain_pty(&rx);
        assert!(
            reply.contains("2026;") && reply.ends_with("$y"),
            "DECRPM should report mode 2026, got {reply:?}"
        );
    }

    #[test]
    fn nel_index_reverse_index() {
        // NEL (ESC E): CR+LF to the next line.
        let (mut t, mut p) = harness(6, 3);
        feed(&mut t, &mut p, b"ab\x1bEcd");
        assert_eq!(row_text(&t, 0), "ab");
        assert_eq!(row_text(&t, 1), "cd");

        // IND (ESC D): down one line, column preserved.
        let (mut t2, mut p2) = harness(6, 3);
        feed(&mut t2, &mut p2, b"X\x1bDY");
        assert_eq!(row_text(&t2, 0), "X");
        assert_eq!(row_text(&t2, 1), " Y", "IND keeps the column");

        // RI (ESC M): up one line, column preserved.
        let (mut t3, mut p3) = harness(6, 3);
        feed(&mut t3, &mut p3, b"\x1b[2;1Hb\x1bMa");
        assert_eq!(row_text(&t3, 0), " a", "RI moved up, kept column");
        assert_eq!(row_text(&t3, 1), "b");
    }

    #[test]
    fn decid_replies_like_da1() {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        feed(&mut t, &mut p, b"\x1bZ"); // DECID
        let reply = drain_pty(&rx);
        assert!(
            reply.starts_with("\x1b[?") && reply.ends_with('c'),
            "DECID should reply like DA1 (CSI ? … c), got {reply:?}"
        );
    }

    #[test]
    fn cursor_blink_mode_emits_event() {
        let (mut t, mut p, rx) = harness_rx(8, 2);
        feed(&mut t, &mut p, b"\x1b[?12h"); // DECSET 12 = cursor blink on
        let got = rx
            .try_iter()
            .any(|ev| matches!(ev, TermEvent::CursorBlinkingChange));
        assert!(got, "?12h should signal a cursor-blink change");
    }

    #[test]
    fn dec_mode_12_toggles_engine_cursor_blink_state() {
        // The companion to the event test above: confirm the engine actually
        // tracks the blink state on `cursor_style().blinking` so the UI can
        // read it live (we honor the *running* program's wish for solid vs.
        // blinking cursor, not just the static config). This is what
        // `Terminal::cursor_blinking()` returns — exercised through the real
        // vte parser so the mode-flip path is real.
        let (mut t, mut p) = harness(8, 2);
        let initial = t.cursor_style().blinking;
        feed(&mut t, &mut p, b"\x1b[?12h"); // request blink
        assert!(
            t.cursor_style().blinking,
            "DEC mode 12 set must turn cursor blink on (was {initial})"
        );
        feed(&mut t, &mut p, b"\x1b[?12l"); // request solid
        assert!(
            !t.cursor_style().blinking,
            "DEC mode 12 reset must turn cursor blink off"
        );
    }

    #[test]
    fn cht_cbt_tab_navigation() {
        // CHT (CSI I): forward N tab stops (default stops every 8).
        let (mut t, mut p) = harness(40, 2);
        feed(&mut t, &mut p, b"\x1b[3I*");
        assert_eq!(
            row_text(&t, 0).chars().nth(24),
            Some('*'),
            "CHT 3 → column 24"
        );
        // CBT (CSI Z): backward N tab stops.
        let (mut t2, mut p2) = harness(40, 2);
        feed(&mut t2, &mut p2, b"\x1b[1;21H\x1b[1ZB");
        assert_eq!(
            row_text(&t2, 0).chars().nth(16),
            Some('B'),
            "CBT 1 from col 20 → column 16"
        );
    }

    #[test]
    fn xtwinops_text_area_size_chars() {
        // XTWINOPS CSI 18 t → report text area size in characters as
        // CSI 8 ; rows ; cols t (DA-style, deterministic — no window needed).
        let (mut t, mut p, rx) = harness_rx(40, 10);
        feed(&mut t, &mut p, b"\x1b[18t");
        assert_eq!(
            drain_pty(&rx),
            "\x1b[8;10;40t",
            "CSI 18 t must report 8;<rows>;<cols>t"
        );
    }

    #[test]
    fn dsr_device_status_ok() {
        // DSR CSI 5 n → "terminal OK" = CSI 0 n (no malfunction).
        let (mut t, mut p, rx) = harness_rx(8, 3);
        feed(&mut t, &mut p, b"\x1b[5n");
        assert_eq!(
            drain_pty(&rx),
            "\x1b[0n",
            "CSI 5 n must reply CSI 0 n (ready)"
        );
    }

    #[test]
    fn da1_primary_attributes_exact_params() {
        // Primary DA (CSI c) must reply exactly with Kettle's truthful feature
        // set: VT2xx-class id + shipped sixel + shipped OSC 52 clipboard.
        let (mut t, mut p, rx) = harness_rx(10, 3);
        feed(&mut t, &mut p, b"\x1b[c");
        assert_eq!(
            drain_pty(&rx),
            crate::event::PRIMARY_DA_REPLY,
            "DA1 reply must advertise VT2xx + sixel + OSC 52"
        );
        // CSI 0 c is an explicit-parameter alias for the same query.
        let (mut t2, mut p2, rx2) = harness_rx(10, 3);
        feed(&mut t2, &mut p2, b"\x1b[0c");
        assert_eq!(
            drain_pty(&rx2),
            crate::event::PRIMARY_DA_REPLY,
            "CSI 0 c == CSI c"
        );
        // DECID is the older ESC Z spelling of Primary DA and should stay in
        // lockstep with CSI c.
        let (mut t3, mut p3, rx3) = harness_rx(10, 3);
        feed(&mut t3, &mut p3, b"\x1bZ");
        assert_eq!(
            drain_pty(&rx3),
            crate::event::PRIMARY_DA_REPLY,
            "DECID == CSI c"
        );
    }

    #[test]
    fn irm_insert_mode_shifts_right() {
        // Default (replace) vs IRM (CSI 4 h): inserting pushes text right.
        let (mut t, mut p) = harness(10, 2);
        feed(&mut t, &mut p, b"ABCD\x1b[1;1H\x1b[4hX");
        assert_eq!(row_text(&t, 0), "XABCD", "IRM inserts, shifting right");
        feed(&mut t, &mut p, b"\x1b[4l\x1b[1;1HZ");
        assert_eq!(row_text(&t, 0), "ZABCD", "4 l → back to replace");
    }

    #[test]
    fn dectcem_cursor_visibility_mode() {
        let (mut t, mut p) = harness(6, 2);
        assert!(t.mode().contains(TermMode::SHOW_CURSOR), "shown by default");
        feed(&mut t, &mut p, b"\x1b[?25l");
        assert!(!t.mode().contains(TermMode::SHOW_CURSOR), "?25 l hides");
        feed(&mut t, &mut p, b"\x1b[?25h");
        assert!(t.mode().contains(TermMode::SHOW_CURSOR), "?25 h shows");
    }

    #[test]
    fn lnm_newline_mode_sets_flag() {
        // CSI 20 h sets LNM; 20 l clears it. (alacritty_terminal tracks the
        // mode but does not itself translate LF→CRLF on output, so only the
        // mode bit — the conformant, observable part — is asserted here.)
        let (mut t, mut p) = harness(8, 2);
        assert!(!t.mode().contains(TermMode::LINE_FEED_NEW_LINE));
        feed(&mut t, &mut p, b"\x1b[20h");
        assert!(t.mode().contains(TermMode::LINE_FEED_NEW_LINE), "20 h sets");
        feed(&mut t, &mut p, b"\x1b[20l");
        assert!(
            !t.mode().contains(TermMode::LINE_FEED_NEW_LINE),
            "20 l clears LNM"
        );
    }

    #[test]
    fn sgr_individual_attribute_resets() {
        // VT conformance gap. SGR `set` codes are well
        // tested (`sgr_truecolor_bold_and_reset`,
        // `sgr_underline_dim_strike`, …) but the individual
        // attribute-*off* codes weren't:
        //   * SGR 22 — normal intensity (clears bold *and* dim)
        //   * SGR 23 — not italic
        //   * SGR 24 — not underlined (clears all underline styles)
        //   * SGR 27 — not reversed
        //   * SGR 29 — not strikethrough
        // These matter for tools that emit nested styling: nvim /
        // tmux / less / `git diff --color` set an attribute, write,
        // unset just that attribute, and continue with the rest of
        // their accumulated SGR state. Without these we'd silently
        // diverge from xterm behavior (cells AFTER the `not X`
        // would carry residual flags).
        //
        // Note on SGR 25 / blink: `alacritty_terminal`'s `Cell::flags`
        // bitfield deliberately doesn't track BLINK (blink is a
        // render-time concern, not a cell attribute). SGR 5 / 25 are
        // accepted at the parser layer but produce no cell-flag
        // change; we don't assert on them here.
        let (mut t, mut p) = harness(20, 2);
        // Stack: bold + dim + italic + underline + reverse + strike.
        // (Skip blink — see note above.)
        feed(&mut t, &mut p, b"\x1b[1;2;3;4;7;9mA");
        let a = &t.grid()[Point::new(Line(0), Column(0))];
        assert!(a.flags.contains(Flags::BOLD), "SGR 1 set");
        assert!(a.flags.contains(Flags::DIM), "SGR 2 set");
        assert!(a.flags.contains(Flags::ITALIC), "SGR 3 set");
        assert!(a.flags.contains(Flags::UNDERLINE), "SGR 4 set");
        assert!(a.flags.contains(Flags::INVERSE), "SGR 7 set");
        assert!(a.flags.contains(Flags::STRIKEOUT), "SGR 9 set");

        // SGR 22 → clears BOTH bold and dim (normal intensity).
        feed(&mut t, &mut p, b"\x1b[22mB");
        let b = &t.grid()[Point::new(Line(0), Column(1))];
        assert!(!b.flags.contains(Flags::BOLD), "SGR 22 clears bold");
        assert!(!b.flags.contains(Flags::DIM), "SGR 22 clears dim");
        // The other flags must still be set.
        assert!(b.flags.contains(Flags::ITALIC), "SGR 22 keeps italic");
        assert!(b.flags.contains(Flags::UNDERLINE), "SGR 22 keeps underline");
        assert!(b.flags.contains(Flags::INVERSE), "SGR 22 keeps inverse");
        assert!(b.flags.contains(Flags::STRIKEOUT), "SGR 22 keeps strikeout");

        // SGR 23 → italic off only.
        feed(&mut t, &mut p, b"\x1b[23mC");
        let c = &t.grid()[Point::new(Line(0), Column(2))];
        assert!(!c.flags.contains(Flags::ITALIC), "SGR 23 clears italic");
        assert!(c.flags.contains(Flags::UNDERLINE), "SGR 23 keeps underline");

        // SGR 24 → underline off (any style).
        feed(&mut t, &mut p, b"\x1b[24mD");
        let d = &t.grid()[Point::new(Line(0), Column(3))];
        assert!(
            !d.flags.contains(Flags::UNDERLINE),
            "SGR 24 clears underline"
        );
        assert!(d.flags.contains(Flags::INVERSE), "SGR 24 keeps inverse");

        // SGR 27 → inverse off.
        feed(&mut t, &mut p, b"\x1b[27mE");
        let e = &t.grid()[Point::new(Line(0), Column(4))];
        assert!(!e.flags.contains(Flags::INVERSE), "SGR 27 clears inverse");
        assert!(e.flags.contains(Flags::STRIKEOUT), "SGR 27 keeps strikeout");

        // SGR 29 → strikeout off.
        feed(&mut t, &mut p, b"\x1b[29mF");
        let f = &t.grid()[Point::new(Line(0), Column(5))];
        assert!(
            !f.flags.contains(Flags::STRIKEOUT),
            "SGR 29 clears strikeout"
        );
    }

    #[test]
    fn app_cursor_and_keypad_modes() {
        let (mut t, mut p) = harness(6, 2);
        feed(&mut t, &mut p, b"\x1b[?1h");
        assert!(t.mode().contains(TermMode::APP_CURSOR), "DECCKM set");
        feed(&mut t, &mut p, b"\x1b=");
        assert!(t.mode().contains(TermMode::APP_KEYPAD), "DECKPAM set");
        feed(&mut t, &mut p, b"\x1b[?1l\x1b>");
        assert!(!t.mode().contains(TermMode::APP_CURSOR));
        assert!(!t.mode().contains(TermMode::APP_KEYPAD), "DECKPNM clears");
    }

    #[test]
    fn mouse_tracking_modes_set_and_clear_flags() {
        let (mut t, mut p) = harness(6, 2);
        feed(&mut t, &mut p, b"\x1b[?1000h");
        assert!(t.mode().contains(TermMode::MOUSE_REPORT_CLICK));
        feed(&mut t, &mut p, b"\x1b[?1002h\x1b[?1006h");
        assert!(t.mode().contains(TermMode::MOUSE_DRAG), "?1002 = drag");
        assert!(t.mode().contains(TermMode::SGR_MOUSE), "?1006 = SGR enc");
        feed(&mut t, &mut p, b"\x1b[?1003h");
        assert!(
            t.mode().contains(TermMode::MOUSE_MOTION),
            "?1003 = any-motion"
        );
        feed(&mut t, &mut p, b"\x1b[?1000l\x1b[?1002l\x1b[?1003l");
        assert!(
            !t.mode().intersects(TermMode::MOUSE_MODE),
            "all tracking off"
        );
    }

    #[test]
    fn placeholder_document_row_applies_scrollback_offset_once() {
        use alacritty_terminal::grid::Scroll;

        let (mut term, mut processor) = history_harness(8, 2, 4);
        feed(
            &mut term,
            &mut processor,
            b"0\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6",
        );
        assert_eq!(term.grid().history_size(), 4);
        assert!(term.grid().history_origin() > 0);

        term.grid_mut()[Point::new(Line(-2), Column(0))].c = placeholder::PLACEHOLDER;
        term.scroll_display(Scroll::Delta(2));
        let expected =
            stable_grid_line_id(term.grid().history_origin(), term.grid().history_size(), -2);
        let cells = Terminal::placeholder_cells_from_term(&term);

        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].0, expected);
        assert_eq!(cells[0].1, 0);
        assert_ne!(
            cells[0].0,
            expected.saturating_sub(term.grid().display_offset() as u64),
            "display_iter already reports a grid-relative negative line"
        );
    }

    // SS2/SS3 single-shift (ESC N / ESC O), HTS (ESC H, custom tab
    // stops), DECSCA/DECSEL selective-erase and LNM LF→CRLF *output*
    // translation are not applied by alacritty_terminal, so no conformance
    // test asserts those behaviors (only LNM's mode bit) — see ROADMAP.
}

#[cfg(test)]
mod teardown_tests {
    use super::*;
    use std::time::Duration;

    /// The OSC 133 prompt-mark ring must (a) dedup against
    /// the most-recent mark, (b) preserve insertion order, and (c) cap at
    /// `MAX_PROMPT_MARKS` by dropping the OLDEST — all with O(1) `pop_front`,
    /// not an O(n) `Vec::drain` on every prompt (the hot reader-thread path).
    #[test]
    fn prompt_mark_ring_dedups_and_caps_oldest_first() {
        use std::collections::VecDeque;
        let mut ring: VecDeque<u64> = VecDeque::new();

        // Dedup: pushing the same most-recent mark twice keeps one.
        push_prompt_mark(&mut ring, 10);
        push_prompt_mark(&mut ring, 10);
        assert_eq!(ring.len(), 1);
        // A different mark appends; a non-adjacent repeat is allowed (the shell
        // genuinely re-prompted at a line it used earlier after scrollback).
        push_prompt_mark(&mut ring, 20);
        push_prompt_mark(&mut ring, 10);
        assert_eq!(ring.iter().copied().collect::<Vec<_>>(), vec![10, 20, 10]);

        // Cap: push well past the limit; length pins at MAX, oldest dropped,
        // newest retained, order preserved.
        let mut ring: VecDeque<u64> = VecDeque::new();
        for i in 0..(MAX_PROMPT_MARKS as u64 + 500) {
            push_prompt_mark(&mut ring, i);
        }
        assert_eq!(ring.len(), MAX_PROMPT_MARKS);
        assert_eq!(*ring.front().unwrap(), 500); // oldest 500 dropped
        assert_eq!(
            *ring.back().unwrap(),
            MAX_PROMPT_MARKS as u64 + 499 // newest kept
        );
    }

    #[test]
    fn prompt_row_ids_survive_growth_and_never_alias_evicted_history() {
        // Before the ring reaches capacity, increasing history and decreasing
        // the relative line cancel out, preserving the document row id.
        assert_eq!(stable_grid_line_id(0, 2, -1), 1);
        assert_eq!(stable_grid_line_id(0, 3, -2), 1);
        // Once capacity is full, the origin advances with each eviction and
        // gives replacement content a new id instead of reusing the old one.
        assert_eq!(stable_grid_line_id(1, 3, -2), 2);
    }

    #[test]
    fn prompt_navigation_prunes_evicted_and_reset_rows() {
        use std::collections::VecDeque;

        // Retained ids are [100, 108): history [100,104), screen [104,108).
        // 99 was evicted; 108 is past the active grid and must also fail
        // closed. The visible top at offset two is row 102.
        let mut ring = VecDeque::from([99, 100, 102, 104, 107, 108]);
        assert_eq!(
            prompt_navigation_offset(&mut ring, 100, 4, 4, 2, true),
            Some(4)
        );
        assert_eq!(
            ring.iter().copied().collect::<Vec<_>>(),
            vec![100, 102, 104, 107]
        );

        // Moving forward from row 102 chooses row 104, which is the active
        // screen top and therefore maps to display offset zero.
        assert_eq!(
            prompt_navigation_offset(&mut ring, 100, 4, 4, 2, false),
            Some(0)
        );

        // A reset advances the origin beyond every old mark; none can alias.
        assert_eq!(
            prompt_navigation_offset(&mut ring, 110, 0, 4, 0, false),
            None
        );
        assert!(ring.is_empty());
    }

    /// Resizing a pane must never lower its scrollback cap.
    ///
    /// The cap is derived by dividing the byte budget by a worst-case per-row
    /// cost at the current width, so it falls as a pane widens — and the grid
    /// enforces a lowered cap by discarding the oldest rows, permanently.
    /// Walked across the exact widths that were measured losing history: 77
    /// columns held 5202 lines, 241 held 1681, and dragging back to 77 restored
    /// none of them.
    #[test]
    fn widening_a_pane_never_lowers_its_scrollback_cap() {
        const LINES: usize = 10_000;
        const BYTES: usize = 10_000_000;
        const ROWS: usize = 28;
        let widths = [77usize, 126, 190, 241, 126, 77];

        // Precondition: the underlying computation really does shrink across
        // this walk. Without it the monotonic wrapper would have nothing to
        // do and this test would pass on a machine where `Cell` was small
        // enough that the budget never binds.
        let raw: Vec<usize> = widths
            .iter()
            .map(|&columns| effective_scrollback_lines(LINES, BYTES, columns, ROWS))
            .collect();
        assert!(
            raw[0] > raw[3],
            "fixture must exercise a shrinking cap: {raw:?}"
        );
        assert!(
            raw[3] < raw[5],
            "and it must recover on narrowing, or the round-trip proves nothing"
        );

        let mut cap = raw[0];
        let start = cap;
        let mut seen = vec![cap];
        for &columns in &widths[1..] {
            let next = scrollback_cap_after_resize(cap, LINES, BYTES, columns, ROWS);
            assert!(
                next >= cap,
                "cap fell from {cap} to {next} at {columns} columns — a resize \
                 must never discard history"
            );
            cap = next;
            seen.push(cap);
        }
        assert_eq!(
            cap, start,
            "a round trip back to the starting width must leave the cap where \
             it began, not where the widest step left it: {seen:?}"
        );

        // A genuinely larger allowance still raises the ceiling — the cap is
        // monotonic, not frozen.
        let narrower = scrollback_cap_after_resize(cap, LINES, BYTES, 40, ROWS);
        assert!(
            narrower > cap,
            "narrowing past the starting width must be allowed to raise the \
             cap ({cap} -> {narrower})"
        );
    }

    /// The other half of that rule. A resize must never lower the cap, but
    /// *editing the setting* is the user asking for exactly that, so the two
    /// paths have to disagree — and the edit path has to reach panes that are
    /// already open, which is what was missing: the budget was read once at
    /// spawn, so the Settings overlay's two scrollback rows wrote a value,
    /// reloaded it, and changed nothing you could see.
    #[test]
    fn an_edited_scrollback_setting_may_lower_a_cap_a_resize_could_not() {
        const BYTES: usize = 10_000_000;
        const COLUMNS: usize = 100;
        const ROWS: usize = 28;

        let generous = effective_scrollback_lines(10_000, BYTES, COLUMNS, ROWS);
        let meagre = effective_scrollback_lines(500, BYTES, COLUMNS, ROWS);
        // Precondition: the two settings must actually differ at this geometry,
        // or neither direction below proves anything.
        assert!(
            meagre < generous,
            "fixture must span a real decrease ({generous} -> {meagre})"
        );

        // A resize refuses to carry the decrease...
        assert_eq!(
            scrollback_cap_after_resize(generous, 500, BYTES, COLUMNS, ROWS),
            generous,
            "a resize must never enact a lower cap, whatever the settings say"
        );
        // ...while `set_scrollback_limits` computes the same lower number the
        // resize path declined to apply, and applies it.
        assert_eq!(
            effective_scrollback_lines(500, BYTES, COLUMNS, ROWS),
            meagre,
            "the edit path's arithmetic is the shared helper, not a second copy"
        );

        // And the setter is wired to that helper rather than to the monotonic
        // resize rule — a guard, because the two are one line apart and reusing
        // the resize wrapper here would silently make the rows inert again.
        let src = super::production_source();
        let body = src
            .split("pub fn set_scrollback_limits(")
            .nth(1)
            .and_then(|rest| rest.split("\n    }\n").next())
            .expect("set_scrollback_limits present");
        assert!(
            body.contains("effective_scrollback_lines(lines, bytes,"),
            "the setting path must compute the cap directly"
        );
        assert!(
            !body.contains("scrollback_cap_after_resize"),
            "the setting path must NOT go through the resize rule, which \
             refuses every decrease"
        );
    }

    #[test]
    fn scrollback_byte_budget_derives_history_lines() {
        let line_bytes = scrollback_line_bytes(100);
        assert!(line_bytes >= 100 * std::mem::size_of::<Cell>());

        assert_eq!(
            effective_scrollback_lines(10_000, 0, 100, 24),
            10_000,
            "0 byte cap preserves line-count-only behavior"
        );
        assert_eq!(
            effective_scrollback_lines(10_000, line_bytes * 34, 100, 24),
            10,
            "byte cap includes visible rows, leftover becomes scrollback"
        );
        assert_eq!(
            effective_scrollback_lines(10_000, line_bytes * 24, 100, 24),
            0,
            "visible screen is protected even when budget leaves no history"
        );
        assert_eq!(
            effective_scrollback_lines(7, line_bytes * 1000, 100, 24),
            7,
            "line-count cap still wins when it is smaller"
        );
    }

    /// Agent-first (A1): `child_exit_code` must surface the child's
    /// real exit status once it exits — `kettle exec` propagates it as its own
    /// process exit code. Spawns a real PTY child that exits 3 and polls.
    #[test]
    fn child_exit_code_propagates_real_status() {
        // Each argv token is space-free so CommandBuilder quoting can't change
        // the command line shape on either OS.
        #[cfg(windows)]
        let argv: Vec<String> = ["cmd.exe", "/c", "exit", "3"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        #[cfg(unix)]
        let argv: Vec<String> = ["/bin/sh", "-c", "exit 3"]
            .iter()
            .map(|s| s.to_string())
            .collect();

        let (tx, rx) = crossbeam_channel::unbounded();
        let waker: Waker = std::sync::Arc::new(|| {});
        let term = match Terminal::new(
            &argv,
            None,
            1000,
            80,
            24,
            8,
            16,
            false,
            CursorShape::Block,
            None,
            tx,
            waker,
        ) {
            Ok(t) => t,
            Err(e) => {
                // Soft-skip in a PTY-less sandbox (existing teardown pattern).
                eprintln!("skipping child_exit_code_propagates_real_status: no PTY ({e})");
                return;
            }
        };
        // The headless run loop MUST forward `PtyWrite` (DA1/DSR/XTGETTCAP
        // query answers) back to the PTY — exactly what `kettle exec` does.
        // Without it the child can park forever under ConPTY: Windows'
        // pseudoconsole withholds the child's clean teardown until the terminal
        // answers its startup cursor-position probe (`ESC[6n`), so `try_wait`
        // never reports an exit. This loop is the canonical A1 drain.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut saw_exit_event = false;
        let code = loop {
            while let Ok(ev) = rx.try_recv() {
                match ev {
                    // Answer the child's terminal queries so it can finish.
                    TermEvent::PtyWrite(s) => term.write(s.as_bytes()),
                    TermEvent::Exit | TermEvent::ChildExit(_) => saw_exit_event = true,
                    _ => {}
                }
            }
            if let Some(c) = term.child_exit_code() {
                break c;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "child did not exit within 15s (reader Exit event seen: {saw_exit_event})"
            );
            std::thread::sleep(Duration::from_millis(15));
        };
        let _ = saw_exit_event;
        assert_eq!(code, 3, "exit status must propagate verbatim");
        assert!(
            term.child_exited(),
            "child_exited agrees once code is known"
        );
    }

    /// v2.30.0/v2.30.1: end-to-end — spawn the DEFAULT shell with auto shell-
    /// integration in a REAL ConPTY and confirm BOTH that it reports cwd via
    /// OSC 7 (`current_dir()` updates) AND that a typed command still EXECUTES
    /// (the shell stays interactive). The latter guards the v2.30.1 fix: v2.30.0
    /// injected kettle.ps1 which captured the prompt as a `FunctionInfo` and
    /// invoked it with `&` — that re-resolved to the new wrapper, recursed,
    /// threw, and PowerShell re-fired the prompt forever (no prompt, no input).
    /// `#[ignore]`d — spawns a real pwsh, Windows-only, timing-dependent. Run:
    /// `cargo test -p kettle-core -- --ignored shell_integration_injects_osc7`.
    #[test]
    #[ignore = "spawns a real pwsh in a ConPTY; Windows-only end-to-end shell-integration check"]
    #[cfg(windows)]
    fn shell_integration_injects_osc7_cwd_for_default_pwsh() {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (otx, orx) = crossbeam_channel::unbounded::<Vec<u8>>();
        let waker: Waker = std::sync::Arc::new(|| {});
        // Empty argv → default shell (pwsh); shell_integration = true → inject.
        // Spawn with cwd = None so `current_dir()` starts None — it can ONLY
        // become Some via a parsed OSC 7 from the injected integration's prompt.
        // That proves the whole pipeline end-to-end: inject the embedded
        // kettle.ps1 -> the prompt emits OSC 7 -> kettle parses + accepts it
        // (host validation passes). No typed input, so there's no PSReadLine
        // input-timing race. (cd tracking uses the very same per-prompt OSC 7.)
        let term = match Terminal::new_with_env_and_output(
            &[],
            None,
            2000,
            0,
            80,
            24,
            8,
            16,
            false,
            CursorShape::Block,
            None,
            "xterm-256color",
            "truecolor",
            &[],
            false,
            true, // shell_integration → inject for pwsh
            tx,
            waker,
            Some(PtyOutputSender::best_effort(otx)),
        ) {
            Ok(t) => t,
            Err(e) => {
                eprintln!("skipping shell_integration test: no PTY ({e})");
                return;
            }
        };
        // Drive the shell + pump device-status replies (DSR/DA) back to the PTY
        // exactly like the App does — without that pump PSReadLine blocks on its
        // startup `ESC[6n` cursor query and never reaches a prompt. Run long
        // enough to (a) see the first OSC 7, then (b) type a command and confirm
        // it EXECUTES — which a broken (infinite-prompt-loop) injection cannot.
        let mut raw: Vec<u8> = Vec::new();
        let mut typed = false;
        let start = std::time::Instant::now();
        loop {
            while let Ok(ev) = rx.try_recv() {
                if let TermEvent::PtyWrite(s) = ev {
                    term.write(s.as_bytes());
                }
            }
            while let Ok(chunk) = orx.try_recv() {
                raw.extend_from_slice(&chunk);
            }
            let secs = start.elapsed().as_secs();
            if secs >= 6 && !typed {
                term.write(b"echo KETTLE_OK_4242\r");
                typed = true;
            }
            if secs >= 12 {
                break;
            }
            std::thread::sleep(Duration::from_millis(80));
        }
        term.write(b"exit\r");
        let text = String::from_utf8_lossy(&raw);
        let osc7_count = text.matches("]7;").count();
        let marker_count = text.matches("KETTLE_OK_4242").count();
        // (1) OSC 7 reached kettle → cwd tracking works.
        assert!(
            term.current_dir().is_some_and(|d| !d.is_empty())
                && term.osc_cwd_seen.load(std::sync::atomic::Ordering::Relaxed),
            "injected pwsh should report its cwd via OSC 7; current_dir={:?}",
            term.current_dir()
        );
        // (2) the typed command EXECUTED (its output echoes the marker) → the
        // shell is interactive, NOT stuck in an infinite prompt loop (the v2.30.0
        // regression: `& $FunctionInfo` recursed, the prompt threw, and PowerShell
        // re-fired it forever — no prompt, no input).
        assert!(
            marker_count >= 2,
            "typed command did not execute — shell not interactive (infinite-\
             prompt-loop regression). marker={marker_count}, osc7={osc7_count}, raw_len={}",
            raw.len()
        );
        // (3) no prompt flood: a recursing/throwing prompt re-fires endlessly,
        // emitting OSC 7 hundreds of times; normal operation emits a handful.
        assert!(
            osc7_count < 50,
            "OSC 7 flood ({osc7_count}×) — prompt is looping (raw_len={})",
            raw.len()
        );
    }

    /// Portable ordering model for legacy ConPTY teardown. Closing a
    /// pseudoconsole can emit final output and wait for the host to consume
    /// it, so the cooperative reader stop must remain false until close
    /// returns.
    #[test]
    fn pty_close_keeps_reader_live_until_close_returns() {
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let stop_for_reader = std::sync::Arc::clone(&stop);
        let (final_output_tx, final_output_rx) = crossbeam_channel::bounded(1);
        let (drained_tx, drained_rx) = crossbeam_channel::bounded(1);

        let reader = std::thread::spawn(move || {
            final_output_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("close published final PTY output");
            assert!(
                !stop_for_reader.load(Ordering::Relaxed),
                "reader stop was published before PTY close completed"
            );
            drained_tx.send(()).expect("acknowledge final PTY output");
        });

        close_pty_while_reader_is_live(&stop, || {
            final_output_tx
                .send(())
                .expect("simulate final output from PTY close");
            drained_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("reader remained live to drain final PTY output");
        });

        reader.join().expect("reader model thread");
        assert!(
            stop.load(Ordering::Relaxed),
            "reader stop must publish after PTY close returns"
        );
    }

    #[cfg(windows)]
    #[test]
    fn asynchronous_conpty_close_owns_reader_stop_across_terminal_drop() {
        let stop = AtomicBool::new(false);
        let close = ConPtyCloseState::default();

        close.terminal_dropped(&stop);
        assert!(
            !stop.load(Ordering::Acquire),
            "dropping Terminal while ClosePseudoConsole is blocked must keep its reader alive"
        );
        close.close_completed(&stop);
        assert!(
            stop.load(Ordering::Acquire),
            "the close owner must stop the reader after the real close returns"
        );

        let stop = AtomicBool::new(false);
        let close = ConPtyCloseState::default();
        close.close_completed(&stop);
        assert!(
            !stop.load(Ordering::Acquire),
            "ordinary close completion still lets the parser consume its EOF marker"
        );
        close.terminal_dropped(&stop);
        assert!(
            stop.load(Ordering::Acquire),
            "a later Terminal drop must observe that close already completed"
        );
    }

    /// A full parser handoff must not pin the blocking pump during teardown.
    /// Once drain mode is published, the pump recovers its buffer and can
    /// continue consuming conout without waiting for parser progress.
    #[test]
    fn full_pump_queue_yields_to_teardown_drain() {
        let (raw_tx, raw_rx) = crossbeam_channel::bounded(1);
        raw_tx
            .send(Some(vec![1]))
            .expect("fill bounded parser handoff");

        let drain_output = std::sync::Arc::new(AtomicBool::new(false));
        let drain_for_pump = std::sync::Arc::clone(&drain_output);
        let start = std::sync::Arc::new(std::sync::Barrier::new(2));
        let pump_start = std::sync::Arc::clone(&start);
        let (done_tx, done_rx) = crossbeam_channel::bounded(1);
        let pump = std::thread::spawn(move || {
            pump_start.wait();
            let result = forward_pty_buffer_or_drain(&raw_tx, &drain_for_pump, vec![2]);
            done_tx.send(result).expect("publish pump handoff result");
        });

        start.wait();
        // Let the sender observe at least one full-queue timeout before
        // switching it into teardown drain mode.
        std::thread::sleep(Duration::from_millis(25));
        drain_output.store(true, Ordering::Release);

        let drained = done_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("full pump queue must yield to teardown drain");
        assert_eq!(drained, PtyPumpSend::Drain(vec![2]));
        assert_eq!(
            raw_rx.recv().expect("original queued parser chunk"),
            Some(vec![1])
        );
        pump.join().expect("pump handoff model thread");
    }

    #[test]
    fn source_progress_tracks_chunks_hidden_in_the_parser_pipeline() {
        let progress = PtyReadProgressState::new();

        progress.mark_chunk_read();
        progress.mark_chunk_read();
        assert_eq!(
            progress.load(),
            PtyReadProgress {
                status: PtyReadStatus::Reading,
                generation: 2,
                pending_chunks: 2,
            }
        );

        progress.mark_chunk_handled();
        assert_eq!(progress.load().pending_chunks, 1);
        progress.mark_chunk_handled();
        assert_eq!(progress.load().pending_chunks, 0);
        progress.set_status(PtyReadStatus::Eof);
        assert_eq!(progress.load().status, PtyReadStatus::Eof);
    }

    /// Regression guard (runtime). Dropping a `Terminal` whose
    /// child is alive and whose PTY reader is parked in a blocking `read()`
    /// must return PROMPTLY. The previous `Drop` `join()`ed the reader while the
    /// master was still open; on Windows ConPTY that join could never
    /// complete, so the UI thread (which owns the drop on a pane close)
    /// deadlocked and the window went "not responding". We run the drop on a
    /// worker thread and require it to finish far inside the old hang window.
    #[test]
    fn drop_is_prompt_with_blocked_reader() {
        // A child that stays alive and quiet, so the reader is parked in a
        // blocking read at drop time: `cmd.exe` waits on stdin; `cat` (no
        // args) blocks reading stdin and emits nothing.
        #[cfg(windows)]
        let argv = vec!["cmd.exe".to_string()];
        #[cfg(unix)]
        let argv = vec!["/bin/cat".to_string()];

        let (tx, _rx) = crossbeam_channel::unbounded();
        let waker: Waker = std::sync::Arc::new(|| {});
        let term = match Terminal::new(
            &argv,
            None,
            1000,
            80,
            24,
            8,
            16,
            false,
            CursorShape::Block,
            None,
            tx,
            waker,
        ) {
            Ok(t) => t,
            // A sandbox without a usable PTY (rare on the CI runners) — soft
            // skip rather than red the suite. The deterministic source drift
            // guard below pins the invariant without needing a real PTY.
            Err(e) => {
                eprintln!("skipping drop_is_prompt_with_blocked_reader: no PTY ({e})");
                return;
            }
        };

        // Let the child start and the reader settle into a blocking read.
        std::thread::sleep(Duration::from_millis(300));

        let (done_tx, done_rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            drop(term);
            let _ = done_tx.send(());
        });

        assert!(
            done_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "Terminal::Drop blocked >5s — the reader-thread join \
             deadlock has regressed (Drop must detach the reader, never join)"
        );
    }

    /// Native Windows regression for the pre-24H2 failure shape: the child is
    /// producing enough output to keep conout active when the pane is dropped.
    /// UI-side `Drop` must return promptly, while the detached reaper must also
    /// finish (which requires the reader to remain live through
    /// `ClosePseudoConsole` on older Windows).
    #[test]
    #[cfg(windows)]
    fn high_output_drop_returns_promptly_and_reaper_finishes() {
        let argv = vec![
            "cmd.exe".to_string(),
            "/d".to_string(),
            "/q".to_string(),
            "/c".to_string(),
            "for /L %i in (1,1,2147483647) do @echo 0123456789abcdef0123456789abcdef".to_string(),
        ];

        let (tx, rx) = crossbeam_channel::unbounded();
        let waker: Waker = std::sync::Arc::new(|| {});
        let term = match Terminal::new(
            &argv,
            None,
            1000,
            80,
            24,
            8,
            16,
            false,
            CursorShape::Block,
            None,
            tx,
            waker,
        ) {
            Ok(term) => term,
            Err(error) => {
                eprintln!(
                    "skipping high_output_drop_returns_promptly_and_reaper_finishes: \
                     no ConPTY ({error})"
                );
                return;
            }
        };

        // Answer ConPTY's startup terminal queries until enough output has
        // crossed the bounded pump to prove this is the high-output path.
        let output_deadline = std::time::Instant::now() + Duration::from_secs(10);
        while term.output_generation() < (PTY_PUMP_QUEUE_DEPTH as u64 + 2) {
            while let Ok(event) = rx.try_recv() {
                if let TermEvent::PtyWrite(reply) = event {
                    term.write(reply.as_bytes());
                }
            }
            assert!(
                std::time::Instant::now() < output_deadline,
                "high-output ConPTY child produced no sustained output"
            );
            std::thread::sleep(Duration::from_millis(5));
        }

        // The reaper owns the only extra child Arc after `Terminal` has
        // finished dropping, so its disappearance is an observable completion
        // signal without adding production-only teardown state.
        let child = std::sync::Arc::clone(&term.child);
        let stop = std::sync::Arc::clone(&term.stop);
        assert_eq!(std::sync::Arc::strong_count(&child), 2);

        let (drop_tx, drop_rx) = crossbeam_channel::bounded(1);
        std::thread::spawn(move || {
            drop(term);
            let _ = drop_tx.send(());
        });

        assert!(
            drop_rx.recv_timeout(Duration::from_secs(5)).is_ok(),
            "Terminal::Drop blocked the caller during high-output teardown"
        );

        let reaper_deadline = std::time::Instant::now() + Duration::from_secs(30);
        while std::sync::Arc::strong_count(&child) != 1
            && std::time::Instant::now() < reaper_deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(
            std::sync::Arc::strong_count(&child),
            1,
            "detached PTY reaper did not finish after high-output close"
        );
        assert!(
            stop.load(Ordering::Relaxed),
            "reader stop was not published after native PTY close"
        );
    }

    /// Regression guard (source, deterministic / cross-platform).
    /// `Terminal::Drop` must DETACH the reader thread, never `join()` it: a
    /// future refactor that re-adds `.join()` reintroduces the Windows
    /// UI-thread deadlock. Inspect just the `fn drop` body so doc comments
    /// and surrounding code can't skew the check.
    #[test]
    fn drop_detaches_reader_never_joins() {
        // Normalize CRLF→LF first: the repo checks out with Windows line
        // endings, so byte patterns must not assume bare `\n`.
        let src = super::production_source();
        // Anchor on the impl, not on the first `fn drop` in the file. There is
        // more than one `Drop` in this module, and which one comes first is an
        // accident of ordering — this test silently retargeted itself the
        // moment another `Drop` was added above `Terminal`'s.
        let impl_start = src
            .find("impl Drop for Terminal {")
            .expect("Terminal::Drop present");
        let start = impl_start
            + src[impl_start..]
                .find("fn drop(&mut self) {")
                .expect("Terminal::Drop body present");
        let rest = &src[start..];
        // The fn body closes at a 4-space-indented `}`; every nested block
        // inside closes at >=8 spaces, so the first `\n    }` is unambiguous.
        let end = rest.find("\n    }").map(|e| e + 5).expect("drop fn close");
        let body = &rest[..end];
        assert!(
            body.contains("reader_thread.take()"),
            "Drop must take() (detach) the reader thread handle"
        );
        assert!(
            !body.contains(".join("),
            "Terminal::Drop must NOT join the PTY reader — joining on the UI \
             thread deadlocks on a blocked ConPTY read"
        );
        let reaper_spawn = body
            .find(".spawn(move ||")
            .expect("PTY reaper closure present");
        let drain_publish = body
            .find("self.drain_output.store(true")
            .expect("teardown drain mode publication present");
        assert!(
            drain_publish < reaper_spawn,
            "Drop must switch the pump to drain mode before detached PTY close"
        );
        assert!(
            !body[..reaper_spawn].contains("stop.store("),
            "Drop must not stop the reader before the detached PTY close starts"
        );
        assert!(
            body.contains("close_pty_while_reader_is_live"),
            "PTY reaper must keep the output reader live through master close"
        );
        // Drop must also close the possibly-blocking pseudoconsole and reap the
        // killed child off-thread so neither ConPTY nor a Unix zombie can
        // block/leak on the UI path.
        assert!(
            body.contains("kettle-pty-reaper")
                && body.contains("drop(master);")
                && body.contains("child.wait()"),
            "Drop must close the master and reap the child in a detached worker"
        );
    }

    /// A killer that records whether it was asked to terminate anything.
    #[derive(Debug, Clone)]
    struct RecordingKiller(std::sync::Arc<AtomicBool>);

    impl portable_pty::ChildKiller for RecordingKiller {
        fn kill(&mut self) -> std::io::Result<()> {
            self.0.store(true, Ordering::SeqCst);
            Ok(())
        }

        fn clone_killer(&self) -> Box<dyn portable_pty::ChildKiller + Send + Sync> {
            Box::new(self.clone())
        }
    }

    /// The guard exists so a construction failure after `spawn_command` cannot
    /// leave the process running. Dropping a `Box<dyn Child>` does not
    /// terminate anything, so without this the child simply survived.
    #[test]
    fn an_armed_guard_kills_the_child_it_covers() {
        let killed = std::sync::Arc::new(AtomicBool::new(false));
        drop(SpawnedChildGuard::arm(Box::new(RecordingKiller(
            std::sync::Arc::clone(&killed),
        ))));
        assert!(
            killed.load(Ordering::SeqCst),
            "a construction failure must terminate the child it started"
        );
    }

    /// The other half matters just as much: on the success path the child
    /// belongs to the returned `Terminal`, and killing it there would close
    /// every pane the instant it opened.
    #[test]
    fn a_disarmed_guard_leaves_the_child_alone() {
        let killed = std::sync::Arc::new(AtomicBool::new(false));
        let mut guard =
            SpawnedChildGuard::arm(Box::new(RecordingKiller(std::sync::Arc::clone(&killed))));
        guard.disarm();
        drop(guard);
        assert!(
            !killed.load(Ordering::SeqCst),
            "a completed construction must keep its child"
        );
    }

    #[test]
    fn direct_child_observation_is_opt_in_for_interactive_callers() {
        assert!(
            !TerminalCapabilities::default().observe_child_exit,
            "headless/default terminals must not allocate the UI-only child observer"
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_pty_watcher_wakes_for_master_output() {
        use std::io::Write as _;
        use std::os::unix::net::UnixStream;

        let (reader, mut writer) = UnixStream::pair().expect("create pollable pair");
        let mut watcher = UnixPtyWatcher::new(std::process::id(), reader.as_raw_fd());
        writer.write_all(b"x").expect("make master readable");

        assert_eq!(
            watcher
                .wait(reader.as_raw_fd(), None)
                .expect("watch output"),
            Some(UnixStartupWake::Output)
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_pty_watcher_observes_child_exit_without_reaping_it() {
        use std::os::unix::net::UnixStream;

        let (reader, _writer) = UnixStream::pair().expect("create unreadable pair");
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 0"])
            .spawn()
            .expect("spawn silent child");
        let mut watcher = UnixPtyWatcher::new(child.id(), reader.as_raw_fd());

        assert_eq!(
            watcher
                .wait(reader.as_raw_fd(), None)
                .expect("watch child exit"),
            Some(UnixStartupWake::ChildExit)
        );
        assert_eq!(
            child
                .wait()
                .expect("watcher left status for child owner")
                .code(),
            Some(0)
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_pty_watcher_handles_a_child_that_exited_before_registration() {
        use std::os::unix::net::UnixStream;

        let (reader, _writer) = UnixStream::pair().expect("create unreadable pair");
        let mut child = std::process::Command::new("/bin/sh")
            .args(["-c", "exit 7"])
            .spawn()
            .expect("spawn already-exited child");
        std::thread::sleep(Duration::from_millis(25));
        let mut watcher = UnixPtyWatcher::new(child.id(), reader.as_raw_fd());

        assert_eq!(
            watcher
                .wait(reader.as_raw_fd(), None)
                .expect("observe pre-registration exit"),
            Some(UnixStartupWake::ChildExit)
        );
        assert_eq!(
            child.wait().expect("status remains reapable").code(),
            Some(7)
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_pty_watcher_honors_a_bounded_wait() {
        use std::os::unix::net::UnixStream;

        let (reader, _writer) = UnixStream::pair().expect("create unreadable pair");
        let mut watcher = UnixPtyWatcher::new(std::process::id(), reader.as_raw_fd());
        let deadline = std::time::Instant::now() + Duration::from_millis(20);

        assert_eq!(
            watcher
                .wait(reader.as_raw_fd(), Some(deadline))
                .expect("bounded wait"),
            None
        );
        assert!(
            std::time::Instant::now() >= deadline,
            "the watcher returned before its deadline without an event"
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn simultaneous_kqueue_output_is_read_before_the_retained_slave_is_dropped() {
        use std::io::Read as _;
        use std::os::fd::{AsRawFd as _, FromRawFd as _, OwnedFd};

        let mut master = -1;
        let mut slave = -1;
        assert_eq!(
            unsafe {
                libc::openpty(
                    &mut master,
                    &mut slave,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            },
            0,
            "openpty: {}",
            io::Error::last_os_error()
        );
        let mut gate = [-1; 2];
        assert_eq!(unsafe { libc::pipe(gate.as_mut_ptr()) }, 0);
        let child_pid = unsafe { libc::fork() };
        assert!(child_pid >= 0, "fork: {}", io::Error::last_os_error());
        if child_pid == 0 {
            unsafe {
                libc::close(master);
                libc::close(gate[1]);
                let mut byte = 0u8;
                let _ = libc::read(gate[0], (&mut byte as *mut u8).cast(), 1);
                let _ = libc::write(slave, b"tail".as_ptr().cast(), 4);
                libc::_exit(0);
            }
        }
        // SAFETY: the parent owns each descriptor returned above exactly once.
        let mut master = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(master) });
        let slave = unsafe { OwnedFd::from_raw_fd(slave) };
        let gate_read = unsafe { OwnedFd::from_raw_fd(gate[0]) };
        let gate_write = unsafe { OwnedFd::from_raw_fd(gate[1]) };
        drop(gate_read);
        let mut watcher = UnixPtyWatcher::new(child_pid as u32, master.as_raw_fd());
        assert_eq!(
            unsafe { libc::write(gate_write.as_raw_fd(), b"x".as_ptr().cast(), 1) },
            1
        );
        drop(gate_write);
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while unix_child_exit_code_unreaped(child_pid as u32)
            .expect("observe child without reaping")
            .is_none()
        {
            assert!(std::time::Instant::now() < deadline, "child did not exit");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(
            watcher
                .wait(master.as_raw_fd(), None)
                .expect("wait for both edges"),
            Some(UnixStartupWake::OutputAndChildExit)
        );
        let mut tail = [0u8; 4];
        master
            .read_exact(&mut tail)
            .expect("read before slave release");
        assert_eq!(&tail, b"tail");
        drop(slave);
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(child_pid, &mut status, 0) },
            child_pid
        );
        assert!(libc::WIFEXITED(status));
    }

    #[cfg(windows)]
    #[test]
    fn windows_child_observer_does_not_consume_process_status() {
        let mut child = std::process::Command::new("cmd.exe")
            .args(["/c", "exit", "9"])
            .spawn()
            .expect("spawn short Windows child");
        let observed = Arc::new(Mutex::new(None));
        let lifecycle_pending = Arc::new(AtomicBool::new(false));
        let (wake_tx, wake_rx) = std::sync::mpsc::channel();
        let waker: Waker = Arc::new(move || {
            let _ = wake_tx.send(());
        });
        spawn_windows_child_exit_observer(
            &child,
            Arc::clone(&observed),
            Arc::clone(&lifecycle_pending),
            waker,
        )
        .expect("spawn process-handle observer");

        wake_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("observer wakes on process exit");
        assert!(observed.lock().unwrap().is_some());
        assert!(lifecycle_pending.load(Ordering::Acquire));
        assert!(
            wake_rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "the observer must exit after one process edge instead of retaining a handle and thread for ten seconds"
        );
        assert_eq!(
            portable_pty::Child::wait(&mut child)
                .expect("original handle still owns status")
                .exit_code(),
            9
        );
    }

    /// A daemonized descendant can keep the PTY slave open after the direct
    /// shell exits. The GUI reader must still emit its ordered Exit marker
    /// after the bounded drain rather than leaving Close/Restart/Hold pending
    /// forever. Linux supplies `setsid(1)` for this exact containment escape;
    /// the companion headless test uses the same fixture shape.
    #[cfg(target_os = "linux")]
    #[test]
    fn leaked_slave_cannot_hold_the_terminal_exit_event_forever() {
        struct KillOnDrop(Option<libc::pid_t>);
        impl Drop for KillOnDrop {
            fn drop(&mut self) {
                if let Some(pid) = self.0 {
                    unsafe {
                        libc::kill(-pid, libc::SIGKILL);
                        libc::kill(pid, libc::SIGKILL);
                    };
                }
            }
        }

        // The child writes its own PID before waking the parent. Reporting
        // `$!` after a parent-side sleep only proved that `fork` happened; the
        // parent could exit before the child installed its HUP policy, turning
        // this retained-slave fixture into an ordinary-EOF fixture.
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "ready=; trap 'ready=1' USR1; \
             setsid sh -c 'trap \"\" HUP; \
             printf \"LEAKED_SLAVE_PID %s\\n\" \"$$\"; \
             kill -USR1 \"$1\"; exec sleep 30' sh \"$$\" & \
             while [ -z \"$ready\" ]; do sleep 0.01; done"
                .to_string(),
        ];
        let (tx, rx) = crossbeam_channel::unbounded();
        let waker: Waker = Arc::new(|| {});
        let term = Terminal::new(
            &argv,
            None,
            100,
            80,
            24,
            8,
            16,
            false,
            CursorShape::Block,
            None,
            tx,
            waker,
        )
        .expect("spawn leaked-slave fixture");
        let mut cleanup = KillOnDrop(None);
        let deadline =
            std::time::Instant::now() + PTY_CHILD_EXIT_EOF_TIMEOUT + Duration::from_secs(3);
        let mut saw_exit = false;
        while std::time::Instant::now() < deadline && !saw_exit {
            while let Ok(event) = rx.try_recv() {
                match event {
                    TermEvent::PtyWrite(reply) => term.write(reply.as_bytes()),
                    TermEvent::Exit => saw_exit = true,
                    _ => {}
                }
            }
            if cleanup.0.is_none()
                && let Some(screen) = term.screen_text(0)
                && let Some(pid) = screen
                    .text
                    .split("LEAKED_SLAVE_PID ")
                    .nth(1)
                    .and_then(|tail| {
                        tail.chars()
                            .take_while(char::is_ascii_digit)
                            .collect::<String>()
                            .parse::<libc::pid_t>()
                            .ok()
                    })
            {
                cleanup.0 = Some(pid);
            }
            if !saw_exit {
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        let leaked_pid = cleanup
            .0
            .expect("fixture did not report its descendant pid");
        let leaked_stdout = std::fs::read_link(format!("/proc/{leaked_pid}/fd/1"))
            .expect("the detached fixture must still own its PTY stdout");
        assert!(
            leaked_stdout.starts_with("/dev/pts/"),
            "detached fixture stdout is not a PTY slave: {}",
            leaked_stdout.display()
        );
        assert!(
            saw_exit,
            "a retained slave held the terminal Exit event past its bound"
        );
        assert_eq!(term.pty_read_status(), PtyReadStatus::EofTimeout);
        assert_eq!(term.child_exit_code(), Some(0));
    }

    /// Pin the construction order and the Unix ownership handoff that close the
    /// short-child output race. Readiness alone is insufficient: a pump can be
    /// descheduled immediately after sending it, so the parent slave must stay
    /// alive until the pump owns it.
    #[test]
    fn the_pty_reader_owns_the_startup_slave_before_the_parent_releases_it() {
        let src = super::production_source();
        let ready = src
            .find("reader_ready_rx\n            .recv()")
            .expect("constructor waits for PTY reader readiness");
        let spawn = src
            .find("let child = pair.slave.spawn_command(cmd)?;")
            .expect("child spawn present");
        let arm = src
            .find("SpawnedChildGuard::arm(child.clone_killer())")
            .expect("spawn guard armed");
        let handoff = src
            .find(".send((pair.slave, lifecycle_watcher))")
            .expect("Unix startup slave is handed to the pump");
        let receive = src
            .find("match startup_slave_rx.recv()")
            .expect("pump receives the Unix startup slave");
        let combined = src
            .find("Ok(Some(UnixStartupWake::OutputAndChildExit)) => {")
            .expect("combined output/exit arm");
        let read = src
            .find("match reader.read(&mut buffer) {")
            .expect("pump read");
        let release_after_read = src
            .find("Ok(n) => {\n                                            #[cfg(unix)]\n                                            startup_slave_guard.take();")
            .expect("pump releases the Unix startup slave after reading");
        let disarm = src.find("spawned.disarm();").expect("spawn guard disarmed");
        let construct = src
            .find("\n        Ok(Terminal {")
            .expect("terminal construction present");

        assert!(
            receive < ready
                && ready < spawn
                && spawn < arm
                && arm < handoff
                && handoff < disarm
                && disarm < construct,
            "the pump must be ready to receive the slave before child spawn; \
             the child and slave ownership guards stay live through handoff"
        );
        assert!(
            receive < release_after_read,
            "the pump must retain the startup slave until a read returns"
        );
        let combined_arm = &src[combined
            ..src[combined..]
                .find("Ok(Some(UnixStartupWake::ChildExit))")
                .map(|offset| combined + offset)
                .expect("child-only exit arm")];
        assert!(
            combined < read
                && read < release_after_read
                && !combined_arm.contains("startup_slave_guard.take()"),
            "simultaneous output/exit must record exit but keep the slave through the read"
        );
        assert!(
            !src[arm..disarm].contains("return Ok("),
            "a success path that returns without disarming would kill a live \
             terminal's child"
        );
    }
}

#[cfg(test)]
mod login_flag_tests {
    use super::{default_shell_accepts_login_flag, prog_accepts_login_flag};

    /// An explicit `command = …` only gets `-l` for a POSIX
    /// shell — never wsl.exe (where `-l` lists distros) or a Windows-native
    /// shell (pwsh/powershell/cmd reject it).
    #[test]
    fn prog_accepts_login_flag_excludes_wsl_and_windows_shells() {
        // POSIX shells (and unknown progs) honor -l.
        assert!(prog_accepts_login_flag("bash"));
        assert!(prog_accepts_login_flag("/bin/zsh"));
        assert!(prog_accepts_login_flag("/usr/bin/fish"));
        // Windows-native shells reject -l (path + .exe + case variants).
        assert!(!prog_accepts_login_flag("pwsh.exe"));
        assert!(!prog_accepts_login_flag(
            r"C:\Program Files\PowerShell\7\pwsh.exe"
        ));
        assert!(!prog_accepts_login_flag("powershell.exe"));
        assert!(!prog_accepts_login_flag("CMD.EXE"));
        assert!(!prog_accepts_login_flag("cmd"));
        // wsl.exe is excluded (-l there means "list distros").
        assert!(!prog_accepts_login_flag("wsl.exe"));
        assert!(!prog_accepts_login_flag("wsl"));
    }

    /// Drift guard. The spawn path gates the default-shell
    /// `-l` injection on this fn, so pinning its value pins the behavior: `-l`
    /// is POSIX-only and must never reach the Windows default shell.
    #[test]
    fn default_shell_login_flag_is_posix_only() {
        assert_eq!(default_shell_accepts_login_flag(), !cfg!(windows));
        #[cfg(windows)]
        assert!(
            !default_shell_accepts_login_flag(),
            "Windows default shell (pwsh/powershell/cmd) must not get -l"
        );
        #[cfg(not(windows))]
        assert!(
            default_shell_accepts_login_flag(),
            "POSIX default shell honors -l when login-shell=true"
        );
    }
}

#[cfg(all(test, windows))]
mod default_shell_tests {
    use super::{
        POWERSHELL_BOOTSTRAP_MAX_CHARS, POWERSHELL_BOOTSTRAP_PREFIX, POWERSHELL_BOOTSTRAP_SUFFIX,
        POWERSHELL_INTEGRATION, base64_standard, is_powershell, merge_windows_paths,
        overlay_windows_parent_env, pick_windows_default_shell, powershell_integration_bootstrap,
        powershell_integration_command,
    };
    use portable_pty::CommandBuilder;
    use std::ffi::{OsStr, OsString};
    use std::path::{Path, PathBuf};

    const PWSH: &str = r"C:\Program Files\PowerShell\7\pwsh.exe";
    const WPS: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";

    /// v2.29.1: the self-contained base64 encoder matches known vectors (RFC 4648).
    #[test]
    fn base64_standard_matches_known_vectors() {
        assert_eq!(base64_standard(b""), "");
        assert_eq!(base64_standard(b"f"), "Zg==");
        assert_eq!(base64_standard(b"fo"), "Zm8=");
        assert_eq!(base64_standard(b"foo"), "Zm9v");
        assert_eq!(base64_standard(b"Man"), "TWFu");
        assert_eq!(base64_standard(b"hi"), "aGk=");
    }

    #[test]
    fn powershell_bootstrap_is_utf8_safe_and_below_the_windows_limit() {
        let bootstrap = powershell_integration_bootstrap("Aé");
        assert_eq!(
            bootstrap,
            format!("{POWERSHELL_BOOTSTRAP_PREFIX}QcOp{POWERSHELL_BOOTSTRAP_SUFFIX}")
        );

        let bootstrap = powershell_integration_bootstrap(POWERSHELL_INTEGRATION);
        assert!(bootstrap.len() <= POWERSHELL_BOOTSTRAP_MAX_CHARS);
        assert_eq!(
            bootstrap.matches(POWERSHELL_BOOTSTRAP_PREFIX).count(),
            1,
            "the fixed decoder must appear exactly once"
        );
        assert!(
            bootstrap.is_ascii(),
            "the CreateProcessW payload must stay one UTF-16 unit per byte"
        );

        let command = powershell_integration_command(Path::new(PWSH));
        let argv = command.get_argv();
        assert_eq!(argv[1], "-NoExit");
        assert_eq!(argv[2], "-Command");
        assert_eq!(argv[3], bootstrap);
    }

    /// v2.29.1: PowerShell executables are recognized by basename; cmd / bash are not.
    #[test]
    fn is_powershell_recognizes_pwsh_and_powershell_only() {
        assert!(is_powershell(Path::new(PWSH)));
        assert!(is_powershell(Path::new(WPS)));
        assert!(is_powershell(Path::new("pwsh")));
        assert!(is_powershell(Path::new(r"D:\tools\PowerShell.EXE")));
        assert!(!is_powershell(Path::new(r"C:\Windows\System32\cmd.exe")));
        assert!(!is_powershell(Path::new("/usr/bin/bash")));
    }

    /// Pwsh 7 wins when both it and Windows PowerShell are present
    /// (matches Windows Terminal's default).
    #[test]
    fn prefers_pwsh_over_windows_powershell() {
        let pick = pick_windows_default_shell(|e| match e {
            "pwsh.exe" => Some(PathBuf::from(PWSH)),
            "powershell.exe" => Some(PathBuf::from(WPS)),
            _ => None,
        });
        assert_eq!(pick, Some(PathBuf::from(PWSH)));
    }

    /// Falls back to Windows PowerShell 5.1 when pwsh 7 is not installed.
    #[test]
    fn falls_back_to_windows_powershell() {
        let pick = pick_windows_default_shell(|e| match e {
            "powershell.exe" => Some(PathBuf::from(WPS)),
            _ => None,
        });
        assert_eq!(pick, Some(PathBuf::from(WPS)));
    }

    /// Neither present → None, so the caller falls back to %ComSpec% / cmd.exe.
    #[test]
    fn none_when_neither_present() {
        assert_eq!(pick_windows_default_shell(|_| None), None);
    }

    #[test]
    fn parent_path_stays_first_and_registry_only_entries_are_retained() {
        let merged = merge_windows_paths(
            Some(OsStr::new(r"C:\runtime;C:\Shared")),
            Some(OsStr::new(r"C:\registry;C:\shared\")),
        )
        .unwrap();
        let entries: Vec<_> = std::env::split_paths(&merged).collect();
        assert_eq!(
            entries,
            [r"C:\runtime", r"C:\Shared", r"C:\registry"].map(std::path::PathBuf::from)
        );
        assert_eq!(merge_windows_paths(None, None), None);
    }

    #[test]
    fn parent_environment_overrides_portable_pty_registry_values() {
        let mut cmd = CommandBuilder::new("cmd.exe");
        cmd.env("PATH", r"C:\registry;C:\shared");
        cmd.env("KETTLE_PARENT_TEST", "registry");
        overlay_windows_parent_env(
            &mut cmd,
            [
                (
                    OsString::from("Path"),
                    OsString::from(r"C:\runtime;C:\shared"),
                ),
                (
                    OsString::from("KETTLE_PARENT_TEST"),
                    OsString::from("runtime"),
                ),
            ],
        );

        assert_eq!(
            cmd.get_env("KETTLE_PARENT_TEST"),
            Some(OsStr::new("runtime"))
        );
        assert_eq!(
            cmd.get_env("PATH"),
            Some(OsStr::new(r"C:\runtime;C:\shared;C:\registry"))
        );
    }
}

#[cfg(test)]
mod wsl_launcher_tests {
    use super::is_wsl_launcher;

    /// The `login_shell` `-l` injection must be suppressed for the
    /// WSL launcher (bare name, `.exe`, full path, any case) because
    /// `wsl.exe -l` lists distros instead of opening a shell.
    #[test]
    fn recognizes_wsl_launcher_forms() {
        assert!(is_wsl_launcher("wsl"));
        assert!(is_wsl_launcher("wsl.exe"));
        assert!(is_wsl_launcher("WSL.EXE"));
        assert!(is_wsl_launcher(r"C:\Windows\System32\wsl.exe"));
        assert!(is_wsl_launcher("/mnt/c/Windows/System32/wsl.exe"));
    }

    /// Real shells must NOT be treated as wsl — they still get `-l`.
    #[test]
    fn does_not_match_other_shells() {
        for p in [
            "bash",
            "/bin/zsh",
            "pwsh.exe",
            "powershell.exe",
            "cmd.exe",
            r"C:\Program Files\PowerShell\7\pwsh.exe",
            "wsltty.exe", // stem is "wsltty", not "wsl"
        ] {
            assert!(!is_wsl_launcher(p), "{p} should not match wsl");
        }
    }
}

#[cfg(test)]
mod pty_dim_tests {
    use super::{
        NATIVE_PTY_GRID_MAX, PtyGeometry, clamp_native_pty_grid, clamp_pty_dim,
        image_cells_for_pixels, native_pty_size, native_resize_required,
    };

    #[test]
    fn ordinary_sizes_pass_through() {
        // A typical 4K-wide grid: 8px cells × 480 cols = 3840px, well
        // within u16. The row/col count case uses cell = 1.
        assert_eq!(clamp_pty_dim(1, 200), 200);
        assert_eq!(clamp_pty_dim(8, 480), 3840);
        assert_eq!(clamp_pty_dim(20, 100), 2000); // HiDPI cell
    }

    #[test]
    fn overflowing_product_saturates_instead_of_wrapping() {
        // 30px HiDPI cell × 5000 cols = 150_000 — overflows u16. The old
        // `cell_w * cols as u16` panicked here in debug / wrapped to 18928
        // in release; we clamp to u16::MAX instead.
        assert_eq!(clamp_pty_dim(30, 5000), u16::MAX);
        // Pathological count that would truncate in the old `cols as u16`.
        assert_eq!(clamp_pty_dim(1, usize::MAX), u16::MAX);
        assert_eq!(clamp_pty_dim(10, usize::MAX), u16::MAX);
    }

    #[test]
    fn zero_inputs_are_benign() {
        assert_eq!(clamp_pty_dim(0, 80), 0);
        assert_eq!(clamp_pty_dim(8, 0), 0);
    }

    #[test]
    fn native_grid_clamps_before_the_portable_pty_boundary() {
        assert_eq!(clamp_native_pty_grid(120), 120);
        assert_eq!(
            clamp_native_pty_grid(usize::MAX),
            NATIVE_PTY_GRID_MAX as u16
        );
        #[cfg(windows)]
        assert_eq!(
            clamp_native_pty_grid(i16::MAX as usize + 1),
            i16::MAX as u16
        );
    }

    #[test]
    fn native_size_preserves_exact_total_pixels() {
        let size = native_pty_size(PtyGeometry::new(100, 41, 960, 787));
        assert_eq!((size.cols, size.rows), (100, 41));
        assert_eq!((size.pixel_width, size.pixel_height), (960, 787));
    }

    #[test]
    fn failed_native_resize_remains_retryable() {
        let applied = PtyGeometry::new(80, 24, 640, 384);
        let desired = PtyGeometry::new(120, 40, 1152, 768);
        assert!(native_resize_required(applied, desired));
        // A failure deliberately leaves `applied` unchanged; asking for the
        // same desired geometry must therefore still require a native retry.
        assert!(native_resize_required(applied, desired));
    }

    #[cfg(windows)]
    #[test]
    fn conpty_skips_pixel_only_resize() {
        let applied = PtyGeometry::new(120, 40, 1152, 768);
        let desired = PtyGeometry::new(120, 40, 1153, 768);
        assert!(!native_resize_required(applied, desired));
    }

    #[cfg(not(windows))]
    #[test]
    fn unix_publishes_pixel_only_resize() {
        let applied = PtyGeometry::new(120, 40, 1152, 768);
        let desired = PtyGeometry::new(120, 40, 1153, 768);
        assert!(native_resize_required(applied, desired));
    }

    #[test]
    fn image_placement_uses_exact_fractional_cell_geometry() {
        // 960 / 100 = 9.6 px per cell. Dividing by the rounded 10 px metric
        // incorrectly assigned a 960 px image only 96 cells.
        assert_eq!(image_cells_for_pixels(960, 100, 960), 100);
        assert_eq!(image_cells_for_pixels(96, 100, 960), 10);
        assert_eq!(image_cells_for_pixels(97, 100, 960), 11);
        assert_eq!(image_cells_for_pixels(0, 100, 960), 0);
        assert_eq!(image_cells_for_pixels(960, 0, 960), 0);
        assert_eq!(
            image_cells_for_pixels(u32::MAX, usize::MAX, 1),
            usize::MAX,
            "hostile image/grid products must saturate rather than wrap"
        );
    }
}

#[cfg(test)]
mod kitty_placement_geometry_tests {
    use super::{
        ImageSourceCrop, ImageSourceRect, Placement, PlacementParams, PtyGeometry,
        kitty_cursor_movement, recompute_kitty_placements, resolve_kitty_placement,
    };
    use crate::ImageData;

    fn image(width: u32, height: u32) -> ImageData {
        ImageData::new(width, height, vec![0; width as usize * height as usize * 4])
            .expect("test image")
    }

    fn close(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 0.0001, "{actual} != {expected}");
    }

    #[test]
    fn crop_is_intersected_with_the_source_before_auto_sizing() {
        let resolved = resolve_kitty_placement(
            &image(100, 50),
            PlacementParams {
                source_x: 80,
                source_y: 10,
                source_width: 50,
                ..PlacementParams::default()
            },
            PtyGeometry::new(10, 5, 100, 50),
        )
        .expect("visible crop");
        assert_eq!(
            resolved.source_rect,
            Some(ImageSourceRect {
                x: 80,
                y: 10,
                width: 20,
                height: 40,
            })
        );
        assert_eq!((resolved.cell_cols, resolved.cell_rows), (2, 4));
        close(resolved.display_cols, 2.0);
        close(resolved.display_rows, 4.0);

        assert!(
            resolve_kitty_placement(
                &image(10, 10),
                PlacementParams {
                    source_x: 10,
                    ..PlacementParams::default()
                },
                PtyGeometry::new(10, 10, 100, 100),
            )
            .is_none(),
            "an empty source intersection must not create or move a placement"
        );
    }

    #[test]
    fn explicit_size_ends_at_cell_boundaries_after_offsets() {
        let resolved = resolve_kitty_placement(
            &image(20, 10),
            PlacementParams {
                columns: 3,
                rows: 2,
                cell_x_offset: 3,
                cell_y_offset: 4,
                ..PlacementParams::default()
            },
            PtyGeometry::new(10, 5, 100, 50),
        )
        .expect("placement");
        assert_eq!((resolved.cell_cols, resolved.cell_rows), (3, 2));
        close(resolved.x_offset_cells, 0.3);
        close(resolved.y_offset_cells, 0.4);
        close(resolved.display_cols, 2.7);
        close(resolved.display_rows, 1.6);
    }

    #[test]
    fn one_axis_auto_uses_aspect_ratio_and_keeps_effective_bounds_distinct() {
        let rows_only = resolve_kitty_placement(
            &image(20, 10),
            PlacementParams {
                rows: 2,
                cell_y_offset: 3,
                ..PlacementParams::default()
            },
            PtyGeometry::new(10, 10, 100, 100),
        )
        .expect("rows-only placement");
        assert_eq!((rows_only.cell_cols, rows_only.cell_rows), (5, 2));
        close(rows_only.display_cols, 3.4);
        close(rows_only.display_rows, 1.7);

        let columns_only = resolve_kitty_placement(
            &image(20, 10),
            PlacementParams {
                columns: 2,
                cell_x_offset: 3,
                ..PlacementParams::default()
            },
            PtyGeometry::new(10, 10, 100, 100),
        )
        .expect("columns-only placement");
        assert_eq!((columns_only.cell_cols, columns_only.cell_rows), (2, 2));
        close(columns_only.display_cols, 1.7);
        close(columns_only.display_rows, 0.85);
    }

    #[test]
    fn natural_size_recomputes_after_a_monitor_pixel_geometry_change() {
        let img = image(96, 40);
        let params = PlacementParams::default();
        let first =
            resolve_kitty_placement(&img, params, PtyGeometry::new(100, 40, 960, 800)).unwrap();
        let second =
            resolve_kitty_placement(&img, params, PtyGeometry::new(100, 40, 1200, 800)).unwrap();
        assert_eq!((first.cell_cols, first.cell_rows), (10, 2));
        assert_eq!((second.cell_cols, second.cell_rows), (8, 2));
        close(first.display_cols, 10.0);
        close(second.display_cols, 8.0);

        let mut placements = vec![Placement {
            abs_line: 2,
            col: 3,
            cell_cols: first.cell_cols,
            cell_rows: first.cell_rows,
            x_offset_cells: first.x_offset_cells,
            y_offset_cells: first.y_offset_cells,
            display_cols: first.display_cols,
            display_rows: first.display_rows,
            img,
            source_rect: first.source_rect,
            source_crop: None,
            id: Some(7),
            placement_id: 9,
            kitty_params: Some(params),
            z: 0,
        }];
        recompute_kitty_placements(&mut placements, PtyGeometry::new(100, 40, 1200, 800));
        assert_eq!(
            (placements[0].cell_cols, placements[0].cell_rows),
            (8, 2),
            "stored raw Kitty parameters must be re-resolved on a monitor/DPI change"
        );
        close(placements[0].display_cols, 8.0);
    }

    #[test]
    fn cropped_natural_size_recomputes_without_restoring_discarded_source_rows() {
        let img = image(96, 40);
        let params = PlacementParams::default();
        let initial =
            resolve_kitty_placement(&img, params, PtyGeometry::new(100, 40, 960, 800)).unwrap();
        let crop = ImageSourceCrop {
            top: 0.5,
            bottom: 1.0,
        };
        let mut placements = vec![Placement {
            abs_line: 77,
            col: 4,
            cell_cols: initial.cell_cols,
            cell_rows: 1,
            x_offset_cells: initial.x_offset_cells,
            y_offset_cells: 0.0,
            display_cols: initial.display_cols,
            display_rows: initial.display_rows * (crop.bottom - crop.top),
            img,
            source_rect: initial.source_rect,
            source_crop: Some(crop),
            id: Some(7),
            placement_id: 9,
            kitty_params: Some(params),
            z: 0,
        }];

        recompute_kitty_placements(&mut placements, PtyGeometry::new(100, 40, 1200, 1000));

        let retained = &placements[0];
        assert_eq!((retained.abs_line, retained.col), (77, 4));
        assert_eq!(retained.source_crop, Some(crop));
        assert_eq!(retained.kitty_params, Some(params));
        assert_eq!((retained.cell_cols, retained.cell_rows), (8, 1));
        close(retained.display_cols, 8.0);
        close(retained.display_rows, 0.8);
    }

    #[test]
    fn cropped_one_axis_auto_recomputes_width_but_preserves_scrolled_y_offset() {
        let img = image(120, 100);
        let params = PlacementParams {
            source_x: 10,
            source_y: 20,
            source_width: 80,
            source_height: 60,
            columns: 4,
            cell_x_offset: 3,
            cell_y_offset: 4,
            ..PlacementParams::default()
        };
        let initial =
            resolve_kitty_placement(&img, params, PtyGeometry::new(100, 40, 1000, 800)).unwrap();
        let crop = ImageSourceCrop {
            top: 0.2,
            bottom: 0.6,
        };
        let preserved_y_offset = 0.125;
        let mut placements = vec![Placement {
            abs_line: 121,
            col: 6,
            cell_cols: initial.cell_cols,
            cell_rows: 1,
            x_offset_cells: initial.x_offset_cells,
            y_offset_cells: preserved_y_offset,
            display_cols: initial.display_cols,
            display_rows: initial.display_rows * (crop.bottom - crop.top),
            img,
            source_rect: initial.source_rect,
            source_crop: Some(crop),
            id: Some(8),
            placement_id: 10,
            kitty_params: Some(params),
            z: 0,
        }];

        recompute_kitty_placements(&mut placements, PtyGeometry::new(100, 40, 1200, 1000));

        let retained = &placements[0];
        assert_eq!((retained.abs_line, retained.col), (121, 6));
        assert_eq!(
            retained.source_rect,
            Some(ImageSourceRect {
                x: 10,
                y: 20,
                width: 80,
                height: 60,
            })
        );
        assert_eq!(retained.source_crop, Some(crop));
        assert_eq!(retained.kitty_params, Some(params));
        assert_eq!((retained.cell_cols, retained.cell_rows), (4, 1));
        close(retained.x_offset_cells, 0.25);
        close(retained.y_offset_cells, preserved_y_offset);
        close(retained.display_cols, 3.75);
        close(retained.display_rows, 0.54);
    }

    #[test]
    fn offsets_are_bounded_and_cursor_policy_uses_effective_cells() {
        let resolved = resolve_kitty_placement(
            &image(96, 20),
            PlacementParams {
                cell_x_offset: u32::MAX,
                ..PlacementParams::default()
            },
            PtyGeometry::new(100, 10, 960, 200),
        )
        .expect("bounded placement");
        // 9.6 px cell: the largest integral in-cell offset is 9 px.
        close(resolved.x_offset_cells, 9.0 / 9.6);
        assert_eq!(resolved.cell_cols, 11);

        assert_eq!(
            kitty_cursor_movement(PlacementParams::default(), 5, 3).as_deref(),
            Some("\x1b[5C\x1b[2B")
        );
        assert_eq!(
            kitty_cursor_movement(PlacementParams::default(), 5, 1).as_deref(),
            Some("\x1b[5C")
        );
        assert_eq!(
            kitty_cursor_movement(
                PlacementParams {
                    suppress_cursor_movement: true,
                    ..PlacementParams::default()
                },
                5,
                3,
            ),
            None
        );
    }
}

#[cfg(test)]
mod atomic_geometry_tests {
    use std::sync::{Arc, Barrier, Mutex};

    use alacritty_terminal::Term;
    use alacritty_terminal::term::Config as TermConfig;

    use super::{
        EventProxy, PtyGeometry, SharedTerm, TermSize, VersionedPtyGeometry, commit_local_geometry,
        local_geometry_snapshot,
    };

    #[test]
    fn grid_and_pixel_generations_are_atomic_under_concurrent_resize_and_read() {
        let first = PtyGeometry::new(80, 24, 768, 384);
        let second = PtyGeometry::new(101, 37, 970, 703);
        let (tx, _rx) = crossbeam_channel::unbounded();
        let proxy = EventProxy::new(tx, Arc::new(|| {}));
        let term: SharedTerm = Arc::new(Mutex::new(Term::new(
            TermConfig::default(),
            &TermSize {
                columns: first.columns,
                screen_lines: first.rows,
            },
            proxy,
        )));
        let geometry = Arc::new(Mutex::new(VersionedPtyGeometry {
            geometry: first,
            generation: 0,
        }));
        let start = Arc::new(Barrier::new(2));

        let writer_term = term.clone();
        let writer_geometry = geometry.clone();
        let writer_start = start.clone();
        let writer = std::thread::spawn(move || {
            writer_start.wait();
            for index in 0..4_000 {
                let desired = if index % 2 == 0 { second } else { first };
                commit_local_geometry(
                    &writer_term,
                    &writer_geometry,
                    desired,
                    Some(TermConfig::default()),
                );
            }
        });

        start.wait();
        let mut last_generation = 0;
        for _ in 0..4_000 {
            let (columns, rows, snapshot, generation) = local_geometry_snapshot(&term, &geometry);
            assert_eq!((columns, rows), (snapshot.columns, snapshot.rows));
            assert!(
                snapshot == first || snapshot == second,
                "snapshot mixed two geometry generations: {snapshot:?}"
            );
            assert!(generation >= last_generation);
            last_generation = generation;
            std::thread::yield_now();
        }
        writer.join().unwrap();
    }
}

#[cfg(test)]
mod output_publish_guard {
    #[test]
    fn reader_sidechannels_share_the_generation_ordered_output_gate() {
        let source = super::production_source();
        let reader_start = source
            .find(".name(\"kettle-pty-reader\"")
            .expect("PTY reader thread present");
        let reader_tail = &source[reader_start..];
        let reader_end = reader_tail
            .find("\n        Ok(Terminal {")
            .expect("PTY reader thread end");
        let reader = &reader_tail[..reader_end];

        assert!(
            !reader.contains("(waker)();"),
            "parser sidechannels must not bypass the per-pane output gate"
        );
        assert!(
            reader.contains("publish_output_if_ready("),
            "PTY reads must publish through the synchronized-output guard"
        );
        let helper_start = source
            .find("fn publish_output_if_ready(")
            .expect("output publication guard present");
        let helper_tail = &source[helper_start..];
        let helper_end = helper_tail
            .find("\n}\n\n")
            .map(|offset| offset + 2)
            .expect("output publication guard end");
        let helper = &helper_tail[..helper_end];
        let pending = helper
            .find("sync_timeout().is_some()")
            .expect("DEC 2026 pending-state check present");
        let generation = helper
            .find("out_gen.fetch_add(")
            .expect("output generation publication present");
        let wake = helper
            .find("output_wake.request();")
            .expect("gated output wake present");
        assert!(
            pending < generation && generation < wake,
            "pending sync must suppress publication; otherwise Release generation precedes wake"
        );
    }

    #[test]
    fn pty_pump_spawn_failure_is_observable_and_closes_the_pane() {
        let source = super::production_source();
        let name = source
            .find(".name(\"kettle-pty-pump\"")
            .expect("PTY pump thread present");
        let start = source[..name]
            .rfind("if let Err(error)")
            .expect("PTY pump spawn error is handled");
        let end = source[name..]
            .find("\n                    loop {")
            .map(|offset| name + offset)
            .expect("outer reader loop follows pump creation");
        let spawn = &source[start..end];
        assert!(
            spawn.contains("log::error!(\"failed to spawn PTY pump thread: {error}\")")
                && spawn.contains("proxy.send_event_exit();")
                && spawn.contains("return;"),
            "thread exhaustion must leave an actionable diagnostic and a normal pane exit"
        );
        assert!(
            !spawn.contains("let _ = std::thread::Builder"),
            "pump creation errors must not be discarded"
        );
    }
}

#[cfg(test)]
mod output_sender_tests {
    use std::time::Duration;

    use super::PtyOutputSender;

    #[test]
    fn best_effort_delivery_drops_when_its_bounded_queue_is_full() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let output = PtyOutputSender::best_effort(tx);
        output.send(vec![1]);
        output.send(vec![2]);

        assert_eq!(rx.recv().unwrap(), vec![1]);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn lossless_delivery_backpressures_until_the_receiver_drains() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let output = PtyOutputSender::lossless(tx);
        output.send(vec![1]);
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        let sender = std::thread::spawn(move || {
            output.send(vec![2]);
            done_tx.send(()).unwrap();
        });

        assert!(done_rx.recv_timeout(Duration::from_millis(20)).is_err());
        assert_eq!(rx.recv().unwrap(), vec![1]);
        done_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        assert_eq!(rx.recv().unwrap(), vec![2]);
        sender.join().unwrap();
    }
}

#[cfg(test)]
mod pty_read_status_tests {
    use super::{PtyReadStatus, pty_read_error_status};

    #[test]
    fn an_unexpected_read_error_is_not_relabelled_as_eof() {
        let error = std::io::Error::other("injected PTY reader failure");
        assert_eq!(pty_read_error_status(&error), PtyReadStatus::Failed);
    }

    #[cfg(unix)]
    #[test]
    fn unix_eio_is_the_platforms_orderly_pty_hangup() {
        let error = std::io::Error::from_raw_os_error(libc::EIO);
        assert_eq!(pty_read_error_status(&error), PtyReadStatus::Eof);
    }

    #[cfg(windows)]
    #[test]
    fn windows_broken_pipe_is_the_platforms_orderly_conpty_hangup() {
        let error = std::io::Error::from_raw_os_error(
            windows_sys::Win32::Foundation::ERROR_BROKEN_PIPE as i32,
        );
        assert_eq!(pty_read_error_status(&error), PtyReadStatus::Eof);
    }
}

#[cfg(test)]
mod sync_update_flush_guard {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use alacritty_terminal::Term;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::Config as TermConfig;
    use alacritty_terminal::vte::ansi::Processor;

    use super::{
        GraphicsEvent, ImageHistoryPruner, PTY_PUMP_QUEUE_DEPTH, SharedTerm, SyncFlushContext,
        SyncGraphicsDispatch, publish_output_if_ready, receive_pty_chunk,
    };
    use crate::event::OutputWakeGate;
    use crate::{EventProxy, Waker};

    struct Size;

    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            4
        }

        fn screen_lines(&self) -> usize {
            4
        }

        fn columns(&self) -> usize {
            40
        }
    }

    fn shared_term() -> SharedTerm {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let proxy = EventProxy::new(tx, Arc::new(|| {}));
        Arc::new(std::sync::Mutex::new(Term::new(
            TermConfig::default(),
            &Size,
            proxy,
        )))
    }

    fn image_pruning_fixture() -> (crate::Images, ImageHistoryPruner) {
        (
            Arc::new(Mutex::new(Vec::new())),
            ImageHistoryPruner::default(),
        )
    }

    fn ignore_sync_graphics(_: SyncGraphicsDispatch<'_>) {}

    #[test]
    fn pending_sync_suppresses_generation_and_wake_until_close() {
        let term = shared_term();
        let mut processor: Processor = Processor::new();
        let generation = AtomicU64::new(0);
        let wakes = Arc::new(AtomicUsize::new(0));
        let wakes_for_callback = wakes.clone();
        let output_wake = OutputWakeGate::new(Arc::new(move || {
            wakes_for_callback.fetch_add(1, Ordering::Relaxed);
        }));

        processor.advance(&mut *term.lock().unwrap(), b"\x1b[?2026hbuffered");
        assert!(!publish_output_if_ready(
            &processor,
            &generation,
            &output_wake
        ));
        assert_eq!(generation.load(Ordering::Acquire), 0);
        assert_eq!(wakes.load(Ordering::Relaxed), 0);

        processor.advance(&mut *term.lock().unwrap(), b"\x1b[?2026l");
        assert!(publish_output_if_ready(
            &processor,
            &generation,
            &output_wake
        ));
        assert_eq!(generation.load(Ordering::Acquire), 1);
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn omitted_sync_terminator_flushes_and_wakes_at_deadline() {
        let term = shared_term();
        let mut processor: Processor = Processor::new();
        {
            let mut term = term.lock().unwrap();
            processor.advance(&mut *term, b"\x1b[?2026h\x1b[2;3Hstale bottom text");
        }
        let deadline = processor
            .sync_timeout()
            .sync_timeout()
            .expect("DEC 2026 opened a synchronized update");

        let (tx, rx) = crossbeam_channel::bounded(PTY_PUMP_QUEUE_DEPTH);
        let delay = deadline
            .saturating_duration_since(std::time::Instant::now())
            .saturating_add(std::time::Duration::from_millis(20));
        let sender = std::thread::spawn(move || {
            std::thread::sleep(delay);
            tx.send(None).unwrap();
        });
        let generation = AtomicU64::new(0);
        let wakes = Arc::new(AtomicUsize::new(0));
        let wakes_for_callback = wakes.clone();
        let waker: Waker = Arc::new(move || {
            wakes_for_callback.fetch_add(1, Ordering::Relaxed);
        });
        let output_wake = OutputWakeGate::new(waker);
        let (images, mut image_pruner) = image_pruning_fixture();
        let graphics_gate = Mutex::new(());
        let mut on_graphics = ignore_sync_graphics;
        let mut sync_flush = SyncFlushContext {
            term: &term,
            images: &images,
            graphics_gate: &graphics_gate,
            image_pruner: &mut image_pruner,
            on_graphics: &mut on_graphics,
            out_gen: &generation,
            output_wake: &output_wake,
        };

        assert!(receive_pty_chunk(&mut processor, &rx, &mut sync_flush).is_none());
        sender.join().unwrap();
        assert!(processor.sync_timeout().sync_timeout().is_none());
        assert_eq!(generation.load(Ordering::Acquire), 1);
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        assert_eq!(
            term.lock().unwrap().grid()[Point::new(Line(1), Column(2))].c,
            's'
        );
    }

    #[test]
    fn expired_sync_flushes_before_a_queued_chunk() {
        let term = shared_term();
        let mut processor: Processor = Processor::new();
        {
            let mut term = term.lock().unwrap();
            processor.advance(&mut *term, b"\x1b[?2026h\x1b[2;3Hexpired text");
        }
        let deadline = processor
            .sync_timeout()
            .sync_timeout()
            .expect("DEC 2026 opened a synchronized update");
        std::thread::sleep(
            deadline
                .saturating_duration_since(std::time::Instant::now())
                .saturating_add(std::time::Duration::from_millis(20)),
        );

        let (tx, rx) = crossbeam_channel::bounded(PTY_PUMP_QUEUE_DEPTH);
        tx.send(Some(b"next chunk".to_vec())).unwrap();
        let generation = AtomicU64::new(0);
        let wakes = Arc::new(AtomicUsize::new(0));
        let wakes_for_callback = wakes.clone();
        let waker: Waker = Arc::new(move || {
            wakes_for_callback.fetch_add(1, Ordering::Relaxed);
        });
        let output_wake = OutputWakeGate::new(waker);
        let (images, mut image_pruner) = image_pruning_fixture();
        let graphics_gate = Mutex::new(());
        let mut on_graphics = ignore_sync_graphics;
        let mut sync_flush = SyncFlushContext {
            term: &term,
            images: &images,
            graphics_gate: &graphics_gate,
            image_pruner: &mut image_pruner,
            on_graphics: &mut on_graphics,
            out_gen: &generation,
            output_wake: &output_wake,
        };

        let chunk = receive_pty_chunk(&mut processor, &rx, &mut sync_flush)
            .expect("queued chunk is preserved after the flush");
        assert_eq!(chunk, b"next chunk");
        assert!(processor.sync_timeout().sync_timeout().is_none());
        assert_eq!(generation.load(Ordering::Acquire), 1);
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        assert_eq!(
            term.lock().unwrap().grid()[Point::new(Line(1), Column(2))].c,
            'e'
        );
    }

    #[test]
    fn sync_eof_flushes_without_waiting_for_the_deadline() {
        let term = shared_term();
        let mut processor: Processor = Processor::new();
        {
            let mut term = term.lock().unwrap();
            processor.advance(&mut *term, b"\x1b[?2026h\x1b[2;3Hfinal text");
        }

        let (tx, rx) = crossbeam_channel::bounded(PTY_PUMP_QUEUE_DEPTH);
        tx.send(None).unwrap();
        let generation = AtomicU64::new(0);
        let wakes = Arc::new(AtomicUsize::new(0));
        let wakes_for_callback = wakes.clone();
        let waker: Waker = Arc::new(move || {
            wakes_for_callback.fetch_add(1, Ordering::Relaxed);
        });
        let output_wake = OutputWakeGate::new(waker);
        let (images, mut image_pruner) = image_pruning_fixture();
        let graphics_gate = Mutex::new(());
        let mut on_graphics = ignore_sync_graphics;
        let mut sync_flush = SyncFlushContext {
            term: &term,
            images: &images,
            graphics_gate: &graphics_gate,
            image_pruner: &mut image_pruner,
            on_graphics: &mut on_graphics,
            out_gen: &generation,
            output_wake: &output_wake,
        };

        assert!(receive_pty_chunk(&mut processor, &rx, &mut sync_flush).is_none());
        assert!(processor.sync_timeout().sync_timeout().is_none());
        assert_eq!(generation.load(Ordering::Acquire), 1);
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        assert_eq!(
            term.lock().unwrap().grid()[Point::new(Line(1), Column(2))].c,
            'f'
        );
    }

    #[test]
    fn sync_eof_applies_buffered_graphics_before_publishing_the_flush() {
        let term = shared_term();
        let mut processor: Processor = Processor::new();
        {
            let mut term = term.lock().unwrap();
            processor.advance(&mut *term, b"\x1b[?2026h\x1b[2J");
            assert!(
                term.take_graphics_events().events.is_empty(),
                "ED2 remains buffered until the synchronized update is flushed"
            );
        }

        let (tx, rx) = crossbeam_channel::bounded(PTY_PUMP_QUEUE_DEPTH);
        tx.send(None).unwrap();
        let generation = AtomicU64::new(0);
        let wakes = Arc::new(AtomicUsize::new(0));
        let wakes_for_callback = wakes.clone();
        let waker: Waker = Arc::new(move || {
            wakes_for_callback.fetch_add(1, Ordering::Relaxed);
        });
        let output_wake = OutputWakeGate::new(waker);
        let (images, mut image_pruner) = image_pruning_fixture();
        let graphics_gate = Mutex::new(());
        let mut observed = Vec::new();
        let mut on_graphics = |dispatch: SyncGraphicsDispatch<'_>| {
            let SyncGraphicsDispatch::Batch(batch) = dispatch else {
                panic!("this fixture does not schedule synchronized graphics markers");
            };
            assert_eq!(
                generation.load(Ordering::Acquire),
                0,
                "graphics must apply before the output generation is published"
            );
            assert_eq!(
                wakes.load(Ordering::Relaxed),
                0,
                "graphics must apply before the render wake is published"
            );
            assert!(!batch.overflowed);
            observed.extend(batch.events);
        };
        let mut sync_flush = SyncFlushContext {
            term: &term,
            images: &images,
            graphics_gate: &graphics_gate,
            image_pruner: &mut image_pruner,
            on_graphics: &mut on_graphics,
            out_gen: &generation,
            output_wake: &output_wake,
        };

        assert!(receive_pty_chunk(&mut processor, &rx, &mut sync_flush).is_none());
        assert_eq!(observed, vec![GraphicsEvent::EraseDisplay]);
        assert_eq!(generation.load(Ordering::Acquire), 1);
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn split_sync_terminator_arriving_before_deadline_does_not_force_flush() {
        let term = shared_term();
        let mut processor: Processor = Processor::new();
        {
            let mut term = term.lock().unwrap();
            processor.advance(&mut *term, b"\x1b[?2026h\x1b[2;3Hupdated");
        }
        let (tx, rx) = crossbeam_channel::bounded(PTY_PUMP_QUEUE_DEPTH);
        tx.send(Some(b"\x1b[?2026l".to_vec())).unwrap();
        let generation = AtomicU64::new(0);
        let waker: Waker = Arc::new(|| panic!("no timeout wake expected"));
        let output_wake = OutputWakeGate::new(waker);
        let (images, mut image_pruner) = image_pruning_fixture();
        let graphics_gate = Mutex::new(());
        let mut on_graphics = ignore_sync_graphics;
        let mut sync_flush = SyncFlushContext {
            term: &term,
            images: &images,
            graphics_gate: &graphics_gate,
            image_pruner: &mut image_pruner,
            on_graphics: &mut on_graphics,
            out_gen: &generation,
            output_wake: &output_wake,
        };

        let close = receive_pty_chunk(&mut processor, &rx, &mut sync_flush)
            .expect("close sequence received");
        processor.advance(&mut *term.lock().unwrap(), &close);

        assert!(processor.sync_timeout().sync_timeout().is_none());
        assert_eq!(generation.load(Ordering::Acquire), 0);
    }

    #[test]
    fn pty_pump_queue_has_a_hard_capacity() {
        let (tx, _rx) = crossbeam_channel::bounded(PTY_PUMP_QUEUE_DEPTH);
        for _ in 0..PTY_PUMP_QUEUE_DEPTH {
            tx.try_send(Some(vec![0; 1])).unwrap();
        }
        assert!(matches!(
            tx.try_send(Some(vec![0; 1])),
            Err(crossbeam_channel::TrySendError::Full(_))
        ));
    }
}

#[cfg(test)]
mod image_lifecycle_tests {
    use super::{
        Animations, BufferGraphicsState, DeferredGraphicsJournal, GraphicsActionContext,
        GraphicsEvent, GraphicsEventBatch, GraphicsRegistries, GraphicsScroll,
        GraphicsScrollDirection, ImageHistoryPruner, Images, InactiveGraphics,
        PTY_PUMP_QUEUE_DEPTH, Placement, PtyGeometry, RelEntry, Relatives, SharedTerm,
        SyncFlushContext, SyncGraphicsContext, SyncGraphicsDispatch, TermSize,
        VersionedPtyGeometry, VirtualEntry, Virtuals, advance_terminal_bytes,
        advance_terminal_bytes_with_commit_hook, apply_graphics_batch, apply_graphics_chunk_at,
        apply_graphics_event, apply_sync_dispatch, chunk_needs_graphics_gate,
        clear_reflowed_regular_placements, commit_local_geometry, finish_deferred_sync,
        receive_pty_chunk, recompute_kitty_placements, scroll_regular_placements,
    };
    use crate::event::OutputWakeGate;
    use crate::{EventProxy, ImageData, Waker};
    use alacritty_terminal::Term;
    use alacritty_terminal::index::{Column, Line, Point};
    use alacritty_terminal::term::Config as TermConfig;
    use alacritty_terminal::vte::ansi::{Processor, SYNC_MARKER_CAPACITY};
    use kettle_vt::{Chunk, Extractor, PlacementParams};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    fn image(channel: u8) -> ImageData {
        ImageData::new(1, 1, vec![channel, 0, 0, 255]).expect("test pixel")
    }

    fn placement(id: u32) -> Placement {
        Placement {
            abs_line: id as u64,
            col: 0,
            cell_cols: 1,
            cell_rows: 1,
            x_offset_cells: 0.0,
            y_offset_cells: 0.0,
            display_cols: 1.0,
            display_rows: 1.0,
            img: image(id as u8),
            source_rect: None,
            source_crop: None,
            id: Some(id),
            placement_id: 1,
            kitty_params: Some(PlacementParams::default()),
            z: 0,
        }
    }

    fn virtual_entry(id: u32) -> VirtualEntry {
        VirtualEntry {
            img: image(id as u8),
            placement_id: 1,
            cols: 1,
            rows: 1,
            z: 0,
        }
    }

    fn relative_entry(id: u32) -> RelEntry {
        RelEntry {
            img: image(id as u8),
            parent_img: 1,
            parent_placement: 1,
            h: 0,
            v: 0,
            z: 0,
            params: PlacementParams::default(),
        }
    }

    fn registries() -> (Images, Virtuals, Animations, Relatives, InactiveGraphics) {
        (
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(HashMap::new())),
            Arc::new(Mutex::new(BufferGraphicsState::default())),
        )
    }

    struct SyncGraphicsHarness {
        term: SharedTerm,
        processor: Processor,
        extractor: Extractor,
        deferred: DeferredGraphicsJournal,
        active_alternate: bool,
        images: Images,
        virtuals: Virtuals,
        anims: Animations,
        relatives: Relatives,
        inactive: InactiveGraphics,
        geometry: Arc<Mutex<VersionedPtyGeometry>>,
    }

    impl SyncGraphicsHarness {
        fn new() -> Self {
            let (tx, _rx) = crossbeam_channel::unbounded();
            let proxy = EventProxy::new(tx, Arc::new(|| {}));
            let term = Arc::new(Mutex::new(Term::new(
                TermConfig::default(),
                &TermSize {
                    columns: 8,
                    screen_lines: 4,
                },
                proxy,
            )));
            let (images, virtuals, anims, relatives, inactive) = registries();
            Self {
                term,
                processor: Processor::new(),
                extractor: Extractor::new(),
                deferred: DeferredGraphicsJournal::new(),
                active_alternate: false,
                images,
                virtuals,
                anims,
                relatives,
                inactive,
                geometry: Arc::new(Mutex::new(VersionedPtyGeometry {
                    geometry: PtyGeometry::new(8, 4, 80, 40),
                    generation: 0,
                })),
            }
        }

        fn feed(&mut self, bytes: &[u8]) {
            let Self {
                term,
                processor,
                extractor,
                deferred,
                active_alternate,
                images,
                virtuals,
                anims,
                relatives,
                inactive,
                geometry,
            } = self;
            extractor.feed_with(bytes, |extractor, chunk| match chunk {
                Chunk::Pass(bytes) => {
                    let mut context = SyncGraphicsContext {
                        active_alternate,
                        deferred,
                        registries: GraphicsRegistries {
                            inactive,
                            images,
                            virtuals,
                            anims,
                            relatives,
                        },
                        actions: GraphicsActionContext {
                            images,
                            virtuals,
                            anims,
                            relatives,
                            geometry,
                        },
                        extractor,
                    };
                    advance_terminal_bytes(processor, term, &bytes, &mut context);
                }
                Chunk::DeferredGraphics(graphics) => deferred.defer(processor, graphics),
                chunk => {
                    let mut term = term.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                    assert!(
                        apply_graphics_chunk_at(
                            &mut term,
                            chunk,
                            GraphicsActionContext {
                                images,
                                virtuals,
                                anims,
                                relatives,
                                geometry,
                            },
                            extractor,
                        ),
                        "unexpected non-graphics extractor chunk"
                    );
                }
            });
        }

        fn active_ids(&self) -> Vec<u32> {
            self.images
                .lock()
                .unwrap()
                .iter()
                .filter_map(|placement| placement.id)
                .collect()
        }

        fn inactive_ids(&self) -> Vec<u32> {
            self.inactive
                .lock()
                .unwrap()
                .placements
                .iter()
                .filter_map(|placement| placement.id)
                .collect()
        }
    }

    fn kitty_image(id: u32, placement: u32, columns: u32, rows: u32) -> Vec<u8> {
        format!("\x1b_Ga=T,i={id},p={placement},f=32,s=1,v=1,c={columns},r={rows};AQIDBA==\x1b\\")
            .into_bytes()
    }

    fn placement_at(id: u32, abs_line: u64, rows: usize) -> Placement {
        let mut placement = placement(id);
        placement.abs_line = abs_line;
        placement.cell_rows = rows;
        placement.display_rows = rows as f32;
        placement
    }

    fn close(actual: f32, expected: f32) {
        assert!((actual - expected).abs() < 1e-6, "{actual} != {expected}");
    }

    #[test]
    fn plain_scroll_and_resize_share_one_graphics_order() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let proxy = EventProxy::new(tx, Arc::new(|| {}));
        let term: SharedTerm = Arc::new(Mutex::new(Term::new(
            TermConfig::default(),
            &TermSize {
                columns: 8,
                screen_lines: 4,
            },
            proxy,
        )));
        let mut processor = Processor::new();
        {
            let mut term = term.lock().unwrap();
            processor.advance(&mut *term, b"\x1b[4;1H");
            assert!(term.take_graphics_events().events.is_empty());
        }

        let (images, virtuals, anims, relatives, inactive) = registries();
        images.lock().unwrap().push(placement_at(1, 0, 1));
        let geometry = Arc::new(Mutex::new(VersionedPtyGeometry {
            geometry: PtyGeometry::new(8, 4, 80, 40),
            generation: 0,
        }));
        let graphics_gate = Arc::new(Mutex::new(()));
        let order = Arc::new(Mutex::new(Vec::new()));
        let (text_committed_tx, text_committed_rx) = std::sync::mpsc::channel();
        let (resize_committed_tx, resize_committed_rx) = std::sync::mpsc::channel();
        let (reader_done_tx, reader_done_rx) = std::sync::mpsc::channel();

        let reader_term = term.clone();
        let reader_images = images.clone();
        let reader_virtuals = virtuals.clone();
        let reader_anims = anims.clone();
        let reader_relatives = relatives.clone();
        let reader_inactive = inactive.clone();
        let reader_geometry = geometry.clone();
        let reader_gate = graphics_gate.clone();
        let reader_order = order.clone();
        let reader = std::thread::spawn(move || {
            let chunk = Chunk::Pass(b"\n".to_vec());
            let mut deferred = DeferredGraphicsJournal::new();
            let graphics_related = chunk_needs_graphics_gate(&chunk);
            let _graphics_guard = graphics_related.then(|| {
                reader_gate
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
            });
            let Chunk::Pass(bytes) = chunk else {
                unreachable!("fixture creates a pass chunk")
            };
            let mut extractor = Extractor::new();
            let mut active_alternate = false;
            let mut context = SyncGraphicsContext {
                active_alternate: &mut active_alternate,
                deferred: &mut deferred,
                registries: GraphicsRegistries {
                    inactive: &reader_inactive,
                    images: &reader_images,
                    virtuals: &reader_virtuals,
                    anims: &reader_anims,
                    relatives: &reader_relatives,
                },
                actions: GraphicsActionContext {
                    images: &reader_images,
                    virtuals: &reader_virtuals,
                    anims: &reader_anims,
                    relatives: &reader_relatives,
                    geometry: &reader_geometry,
                },
                extractor: &mut extractor,
            };
            advance_terminal_bytes_with_commit_hook(
                &mut processor,
                &reader_term,
                &bytes,
                &mut context,
                |batch| {
                    let saw_scroll =
                        matches!(batch.events.as_slice(), [GraphicsEvent::Scroll { .. }]);
                    reader_order.lock().unwrap().push("text-scroll");
                    text_committed_tx
                        .send((graphics_related, saw_scroll))
                        .unwrap();
                    if !graphics_related {
                        resize_committed_rx.recv().unwrap();
                    }
                },
            );
            reader_order.lock().unwrap().push("graphics-scroll");
            reader_done_tx.send(()).unwrap();
        });

        let (graphics_related, saw_scroll) = text_committed_rx.recv().unwrap();
        assert!(
            saw_scroll,
            "plain LF must emit a real graphics scroll event"
        );
        {
            let _graphics_guard = graphics_gate
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            let resized = PtyGeometry::new(8, 4, 160, 40);
            commit_local_geometry(&term, &geometry, resized, None);
            recompute_kitty_placements(&mut images.lock().unwrap(), resized);
            order.lock().unwrap().push("graphics-resize");
        }
        if !graphics_related {
            resize_committed_tx.send(()).unwrap();
        }
        reader_done_rx.recv().unwrap();
        reader.join().unwrap();

        assert_eq!(
            *order.lock().unwrap(),
            ["text-scroll", "graphics-scroll", "graphics-resize"],
            "a plain-byte scroll committed outside the graphics gate, allowing \
             resize to split its text and graphics mutations"
        );
    }

    #[test]
    fn authoritative_journal_preserves_mixed_control_order() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let proxy = EventProxy::new(tx, Arc::new(|| {}));
        let mut term = Term::new(
            TermConfig::default(),
            &TermSize {
                columns: 4,
                screen_lines: 4,
            },
            proxy,
        );
        let mut processor: Processor = Processor::new();
        processor.advance(
            &mut term,
            b"\x1b[2J\x1b[?47h\x1b[?47l\x1b[?1049h\x1b[?1049l\x1bc",
        );
        assert_eq!(
            term.take_graphics_events().events,
            vec![
                GraphicsEvent::EraseDisplay,
                GraphicsEvent::EnterAlternate {
                    mode: 47,
                    clear: false,
                },
                GraphicsEvent::LeaveAlternate {
                    mode: 47,
                    clear: false,
                },
                GraphicsEvent::EnterAlternate {
                    mode: 1049,
                    clear: true,
                },
                GraphicsEvent::LeaveAlternate {
                    mode: 1049,
                    clear: false,
                },
                GraphicsEvent::Reset,
            ]
        );
    }

    #[test]
    fn synchronized_images_follow_enter_and_leave_wire_order() {
        let mut harness = SyncGraphicsHarness::new();
        let mut first = b"\x1b[?2026h".to_vec();
        first.extend(kitty_image(1, 1, 1, 1));
        first.extend_from_slice(b"\x1b[?1049h");
        first.extend(kitty_image(2, 1, 1, 1));
        first.extend_from_slice(b"\x1b[?2026l");
        harness.feed(&first);

        assert!(harness.active_alternate);
        assert_eq!(harness.active_ids(), vec![2]);
        assert_eq!(harness.inactive_ids(), vec![1]);

        let mut second = b"\x1b[?2026h".to_vec();
        second.extend(kitty_image(3, 1, 1, 1));
        second.extend_from_slice(b"\x1b[?1049l");
        second.extend(kitty_image(4, 1, 1, 1));
        second.extend_from_slice(b"\x1b[?2026l");
        harness.feed(&second);

        assert!(!harness.active_alternate);
        assert_eq!(harness.active_ids(), vec![1, 4]);
        assert_eq!(harness.inactive_ids(), vec![2, 3]);
    }

    #[test]
    fn synchronized_image_cursor_movement_precedes_later_text_and_stays_invisible_mid_update() {
        let mut harness = SyncGraphicsHarness::new();
        let mut update = b"\x1b[?2026h\x1b[2;2H".to_vec();
        update.extend(kitty_image(9, 1, 2, 2));
        update.extend_from_slice(b"X");
        harness.feed(&update);

        assert!(
            harness.images.lock().unwrap().is_empty(),
            "graphics must remain invisible until the synchronized update commits"
        );
        assert_eq!(
            harness.term.lock().unwrap().grid()[Point::new(Line(2), Column(3))].c,
            ' ',
            "text must remain buffered with the image"
        );

        harness.feed(b"\x1b[?2026l");
        let placements = harness.images.lock().unwrap();
        assert_eq!(placements.len(), 1);
        assert_eq!(
            (
                placements[0].col,
                placements[0].cell_cols,
                placements[0].cell_rows
            ),
            (1, 2, 2)
        );
        drop(placements);
        let term = harness.term.lock().unwrap();
        assert_eq!(term.grid()[Point::new(Line(2), Column(3))].c, 'X');
        assert_eq!(term.grid().cursor.point, Point::new(Line(2), Column(4)));
    }

    #[test]
    fn synchronized_graphics_replay_before_forced_eof_publication() {
        let mut harness = SyncGraphicsHarness::new();
        let mut update = b"\x1b[?2026h\x1b[2;2H".to_vec();
        update.extend(kitty_image(10, 1, 2, 1));
        harness.feed(&update);
        assert!(harness.images.lock().unwrap().is_empty());

        let (tx, rx) = crossbeam_channel::bounded(PTY_PUMP_QUEUE_DEPTH);
        tx.send(None).unwrap();
        let generation = AtomicU64::new(0);
        let wakes = Arc::new(AtomicUsize::new(0));
        let wakes_for_callback = wakes.clone();
        let waker: Waker = Arc::new(move || {
            wakes_for_callback.fetch_add(1, Ordering::Relaxed);
        });
        let output_wake = OutputWakeGate::new(waker);
        let graphics_gate = Mutex::new(());
        let mut image_pruner = ImageHistoryPruner::default();

        let SyncGraphicsHarness {
            term,
            processor,
            extractor,
            deferred,
            active_alternate,
            images,
            virtuals,
            anims,
            relatives,
            inactive,
            geometry,
        } = &mut harness;
        let mut on_graphics = |dispatch: SyncGraphicsDispatch<'_>| {
            assert_eq!(
                generation.load(Ordering::Acquire),
                0,
                "deferred graphics must replay before output publication"
            );
            assert_eq!(
                wakes.load(Ordering::Relaxed),
                0,
                "deferred graphics must replay before the redraw wake"
            );
            let finishes_sync = matches!(&dispatch, SyncGraphicsDispatch::Batch(_));
            let mut context = SyncGraphicsContext {
                active_alternate,
                deferred,
                registries: GraphicsRegistries {
                    inactive,
                    images,
                    virtuals,
                    anims,
                    relatives,
                },
                actions: GraphicsActionContext {
                    images,
                    virtuals,
                    anims,
                    relatives,
                    geometry,
                },
                extractor,
            };
            apply_sync_dispatch(dispatch, &mut context);
            if finishes_sync {
                finish_deferred_sync(&mut context);
            }
        };
        let mut sync_flush = SyncFlushContext {
            term,
            images,
            graphics_gate: &graphics_gate,
            image_pruner: &mut image_pruner,
            on_graphics: &mut on_graphics,
            out_gen: &generation,
            output_wake: &output_wake,
        };

        assert!(receive_pty_chunk(processor, &rx, &mut sync_flush).is_none());
        assert_eq!(generation.load(Ordering::Acquire), 1);
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        let placements = images.lock().unwrap();
        assert_eq!(placements.len(), 1);
        assert_eq!((placements[0].id, placements[0].col), (Some(10), 1));
    }

    #[test]
    fn synchronized_graphics_overflow_fails_closed_and_resets_decoder_state() {
        let mut harness = SyncGraphicsHarness::new();
        harness.feed(b"\x1b[?2026h");
        let mut images = Vec::new();
        for id in 1..=SYNC_MARKER_CAPACITY as u32 + 1 {
            images.extend(kitty_image(id, 1, 1, 1));
        }
        harness.feed(&images);

        assert!(harness.deferred.overflowed);
        assert!(harness.deferred.entries.is_empty());
        assert!(harness.images.lock().unwrap().is_empty());

        harness.feed(b"\x1b[?2026l");
        assert!(!harness.deferred.overflowed);
        assert!(harness.deferred.entries.is_empty());
        assert!(harness.images.lock().unwrap().is_empty());

        harness.feed(b"\x1b_Ga=p,i=1,p=2\x1b\\");
        assert!(
            harness.images.lock().unwrap().is_empty(),
            "overflow recovery must clear deferred kitty image data"
        );
    }

    #[test]
    fn mode_47_preserves_primary_and_alternate_graphics() {
        let (images, virtuals, anims, relatives, inactive) = registries();
        images.lock().unwrap().push(placement(1));
        virtuals.lock().unwrap().insert((1, 1), virtual_entry(1));
        relatives.lock().unwrap().insert((2, 1), relative_entry(2));
        let mut extractor = Extractor::new();
        let mut alternate = false;

        apply_graphics_event(
            GraphicsEvent::EnterAlternate {
                mode: 47,
                clear: false,
            },
            &mut alternate,
            GraphicsRegistries {
                inactive: &inactive,
                images: &images,
                virtuals: &virtuals,
                anims: &anims,
                relatives: &relatives,
            },
            &mut extractor,
        );
        assert!(alternate);
        assert!(images.lock().unwrap().is_empty());
        assert!(virtuals.lock().unwrap().is_empty());
        assert_eq!(inactive.lock().unwrap().placements.len(), 1);

        images.lock().unwrap().push(placement(9));
        virtuals.lock().unwrap().insert((9, 1), virtual_entry(9));
        apply_graphics_event(
            GraphicsEvent::LeaveAlternate {
                mode: 47,
                clear: false,
            },
            &mut alternate,
            GraphicsRegistries {
                inactive: &inactive,
                images: &images,
                virtuals: &virtuals,
                anims: &anims,
                relatives: &relatives,
            },
            &mut extractor,
        );
        assert!(!alternate);
        assert_eq!(images.lock().unwrap()[0].id, Some(1));
        assert!(virtuals.lock().unwrap().contains_key(&(1, 1)));
        assert!(!virtuals.lock().unwrap().contains_key(&(9, 1)));
        assert_eq!(inactive.lock().unwrap().placements[0].id, Some(9));

        apply_graphics_event(
            GraphicsEvent::EnterAlternate {
                mode: 47,
                clear: false,
            },
            &mut alternate,
            GraphicsRegistries {
                inactive: &inactive,
                images: &images,
                virtuals: &virtuals,
                anims: &anims,
                relatives: &relatives,
            },
            &mut extractor,
        );
        assert_eq!(images.lock().unwrap()[0].id, Some(9));
        assert!(virtuals.lock().unwrap().contains_key(&(9, 1)));
        assert_eq!(inactive.lock().unwrap().placements[0].id, Some(1));
    }

    #[test]
    fn mode_1047_clears_on_exit_and_1049_clears_on_entry() {
        let (images, virtuals, anims, relatives, inactive) = registries();
        let mut extractor = Extractor::new();
        let mut alternate = false;

        // Seed a persistent alternate buffer through mode 47.
        apply_graphics_event(
            GraphicsEvent::EnterAlternate {
                mode: 47,
                clear: false,
            },
            &mut alternate,
            GraphicsRegistries {
                inactive: &inactive,
                images: &images,
                virtuals: &virtuals,
                anims: &anims,
                relatives: &relatives,
            },
            &mut extractor,
        );
        images.lock().unwrap().push(placement(7));
        apply_graphics_event(
            GraphicsEvent::LeaveAlternate {
                mode: 47,
                clear: false,
            },
            &mut alternate,
            GraphicsRegistries {
                inactive: &inactive,
                images: &images,
                virtuals: &virtuals,
                anims: &anims,
                relatives: &relatives,
            },
            &mut extractor,
        );

        // Mode 1047 preserves alternate contents on entry, then clears them
        // before returning to the primary buffer.
        apply_graphics_event(
            GraphicsEvent::EnterAlternate {
                mode: 1047,
                clear: false,
            },
            &mut alternate,
            GraphicsRegistries {
                inactive: &inactive,
                images: &images,
                virtuals: &virtuals,
                anims: &anims,
                relatives: &relatives,
            },
            &mut extractor,
        );
        assert_eq!(images.lock().unwrap()[0].id, Some(7));
        apply_graphics_event(
            GraphicsEvent::LeaveAlternate {
                mode: 1047,
                clear: true,
            },
            &mut alternate,
            GraphicsRegistries {
                inactive: &inactive,
                images: &images,
                virtuals: &virtuals,
                anims: &anims,
                relatives: &relatives,
            },
            &mut extractor,
        );
        assert!(inactive.lock().unwrap().placements.is_empty());

        // Re-seed the parked alternate buffer. Mode 1049 clears it before
        // entry, but preserves new contents when returning to primary.
        inactive.lock().unwrap().placements.push(placement(8));
        apply_graphics_event(
            GraphicsEvent::EnterAlternate {
                mode: 1049,
                clear: true,
            },
            &mut alternate,
            GraphicsRegistries {
                inactive: &inactive,
                images: &images,
                virtuals: &virtuals,
                anims: &anims,
                relatives: &relatives,
            },
            &mut extractor,
        );
        assert!(images.lock().unwrap().is_empty());
        images.lock().unwrap().push(placement(9));
        apply_graphics_event(
            GraphicsEvent::LeaveAlternate {
                mode: 1049,
                clear: false,
            },
            &mut alternate,
            GraphicsRegistries {
                inactive: &inactive,
                images: &images,
                virtuals: &virtuals,
                anims: &anims,
                relatives: &relatives,
            },
            &mut extractor,
        );
        assert_eq!(inactive.lock().unwrap().placements[0].id, Some(9));
    }

    #[test]
    fn ed2_is_buffer_local_and_ris_clears_both_graphics_buffers() {
        let (images, virtuals, anims, relatives, inactive) = registries();
        images.lock().unwrap().push(placement(1));
        let mut extractor = Extractor::new();
        let mut alternate = false;
        apply_graphics_event(
            GraphicsEvent::EnterAlternate {
                mode: 47,
                clear: false,
            },
            &mut alternate,
            GraphicsRegistries {
                inactive: &inactive,
                images: &images,
                virtuals: &virtuals,
                anims: &anims,
                relatives: &relatives,
            },
            &mut extractor,
        );
        images.lock().unwrap().push(placement(9));
        apply_graphics_event(
            GraphicsEvent::EraseDisplay,
            &mut alternate,
            GraphicsRegistries {
                inactive: &inactive,
                images: &images,
                virtuals: &virtuals,
                anims: &anims,
                relatives: &relatives,
            },
            &mut extractor,
        );
        assert!(images.lock().unwrap().is_empty());
        assert_eq!(inactive.lock().unwrap().placements[0].id, Some(1));

        apply_graphics_event(
            GraphicsEvent::Reset,
            &mut alternate,
            GraphicsRegistries {
                inactive: &inactive,
                images: &images,
                virtuals: &virtuals,
                anims: &anims,
                relatives: &relatives,
            },
            &mut extractor,
        );
        assert!(!alternate);
        assert!(images.lock().unwrap().is_empty());
        assert!(inactive.lock().unwrap().placements.is_empty());
    }

    #[test]
    fn partial_scroll_up_moves_wholly_contained_images_and_crops_at_top_margin() {
        let images: Images = Arc::new(Mutex::new(vec![
            placement_at(1, 103, 2),
            placement_at(2, 100, 1),
            placement_at(3, 101, 2),
            placement_at(4, 104, 1),
            placement_at(5, 102, 1),
        ]));

        assert!(scroll_regular_placements(
            &images,
            GraphicsScroll {
                direction: GraphicsScrollDirection::Up,
                top: 2,
                bottom: 6,
                lines: 2,
                old_screen_top: 100,
                new_screen_top: 100,
                screen_lines: 8,
            },
        ));

        let images = images.lock().unwrap();
        let find = |id| {
            images
                .iter()
                .find(|placement| placement.id == Some(id))
                .unwrap()
        };
        let clipped = find(1);
        assert_eq!(clipped.abs_line, 102);
        close(clipped.display_rows, 1.0);
        let crop = clipped.source_crop.expect("top crop");
        close(crop.top, 0.5);
        close(crop.bottom, 1.0);
        assert_eq!(
            clipped.kitty_params,
            Some(PlacementParams::default()),
            "partial scrolling must retain Kitty intent for a later DPI recompute"
        );
        assert_eq!(find(2).abs_line, 100, "image above the page stays fixed");
        assert_eq!(
            find(3).abs_line,
            101,
            "image crossing a margin must not scroll"
        );
        assert_eq!(find(4).abs_line, 102);
        assert!(
            images.iter().all(|placement| placement.id != Some(5)),
            "an image moved entirely above the margin is discarded"
        );
    }

    #[test]
    fn partial_scroll_down_crops_at_bottom_margin_and_composes_source_ranges() {
        let mut first = placement_at(1, 103, 2);
        first.source_crop = Some(crate::ImageSourceCrop {
            top: 0.2,
            bottom: 1.0,
        });
        let images: Images = Arc::new(Mutex::new(vec![first, placement_at(2, 104, 1)]));

        assert!(scroll_regular_placements(
            &images,
            GraphicsScroll {
                direction: GraphicsScrollDirection::Down,
                top: 2,
                bottom: 6,
                lines: 2,
                old_screen_top: 100,
                new_screen_top: 100,
                screen_lines: 8,
            },
        ));

        let images = images.lock().unwrap();
        assert_eq!(images.len(), 1);
        let clipped = &images[0];
        assert_eq!(clipped.abs_line, 105);
        close(clipped.display_rows, 1.0);
        let crop = clipped.source_crop.expect("bottom crop");
        close(crop.top, 0.2);
        close(crop.bottom, 0.6);
    }

    #[test]
    fn top_anchored_scroll_preserves_history_and_reanchors_fixed_rows() {
        let images: Images = Arc::new(Mutex::new(vec![
            placement_at(1, 10, 1),
            placement_at(2, 11, 1),
            placement_at(3, 15, 1),
        ]));

        assert!(scroll_regular_placements(
            &images,
            GraphicsScroll {
                direction: GraphicsScrollDirection::Up,
                top: 0,
                bottom: 4,
                lines: 1,
                old_screen_top: 10,
                new_screen_top: 11,
                screen_lines: 6,
            },
        ));

        let images = images.lock().unwrap();
        let find = |id| {
            images
                .iter()
                .find(|placement| placement.id == Some(id))
                .unwrap()
        };
        assert_eq!(
            find(1).abs_line,
            10,
            "top-row image follows text into retained history"
        );
        assert_eq!(find(2).abs_line, 11, "scrolled image keeps its document id");
        assert_eq!(
            find(3).abs_line,
            16,
            "row below the partial region stays at the same viewport position"
        );
    }

    #[test]
    fn coalesced_top_scroll_uses_full_screen_delta_for_graphics_anchors() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let proxy = EventProxy::new(tx, Arc::new(|| {}));
        let mut term = Term::new(
            TermConfig::default(),
            &TermSize {
                columns: 4,
                screen_lines: 4,
            },
            proxy,
        );
        let mut processor: Processor = Processor::new();
        processor.advance(&mut term, b"\x1b[1;2r\x1b[2H\n\n\n\n\n\n\n\n\n\n");
        let batch = term.take_graphics_events();
        assert_eq!(
            batch.events,
            vec![GraphicsEvent::Scroll {
                direction: GraphicsScrollDirection::Up,
                top: 0,
                bottom: 2,
                lines: 2,
                old_screen_top: 0,
                new_screen_top: 10,
                screen_lines: 4,
            }]
        );

        let (images, virtuals, anims, relatives, inactive) = registries();
        images.lock().unwrap().extend([
            placement_at(1, 0, 1),
            placement_at(2, 1, 1),
            placement_at(3, 3, 1),
        ]);
        let mut extractor = Extractor::new();
        let mut alternate = false;
        apply_graphics_batch(
            batch,
            &mut alternate,
            GraphicsRegistries {
                inactive: &inactive,
                images: &images,
                virtuals: &virtuals,
                anims: &anims,
                relatives: &relatives,
            },
            &mut extractor,
        );

        let images = images.lock().unwrap();
        let find = |id| {
            images
                .iter()
                .find(|placement| placement.id == Some(id))
                .unwrap()
        };
        assert_eq!(
            find(1).abs_line,
            0,
            "first scrolled row retains its document id in history"
        );
        assert_eq!(
            find(2).abs_line,
            1,
            "second scrolled row retains its document id in history"
        );
        assert_eq!(
            find(3).abs_line,
            13,
            "fixed row below the region stays at the same viewport position"
        );
    }

    #[test]
    fn journal_overflow_clears_both_buffers_and_resynchronizes_active_screen() {
        let (images, virtuals, anims, relatives, inactive) = registries();
        images.lock().unwrap().push(placement(1));
        inactive.lock().unwrap().placements.push(placement(2));
        let mut extractor = Extractor::new();
        let mut alternate = false;

        apply_graphics_batch(
            GraphicsEventBatch {
                events: Vec::new(),
                overflowed: true,
                alternate_screen: true,
            },
            &mut alternate,
            GraphicsRegistries {
                inactive: &inactive,
                images: &images,
                virtuals: &virtuals,
                anims: &anims,
                relatives: &relatives,
            },
            &mut extractor,
        );

        assert!(alternate);
        assert!(images.lock().unwrap().is_empty());
        assert!(inactive.lock().unwrap().placements.is_empty());
    }

    #[test]
    fn column_reflow_clears_regular_anchors_but_preserves_virtual_data() {
        let (images, virtuals, _anims, relatives, inactive) = registries();
        images.lock().unwrap().push(placement(1));
        virtuals.lock().unwrap().insert((1, 1), virtual_entry(1));
        relatives.lock().unwrap().insert((2, 1), relative_entry(2));
        {
            let mut saved = inactive.lock().unwrap();
            saved.placements.push(placement(3));
            saved.virtuals.insert((3, 1), virtual_entry(3));
            saved.relatives.insert((4, 1), relative_entry(4));
        }

        clear_reflowed_regular_placements(&images, &relatives, &inactive);

        assert!(images.lock().unwrap().is_empty());
        assert!(relatives.lock().unwrap().is_empty());
        assert!(virtuals.lock().unwrap().contains_key(&(1, 1)));
        let saved = inactive.lock().unwrap();
        assert!(saved.placements.is_empty());
        assert!(saved.relatives.is_empty());
        assert!(saved.virtuals.contains_key(&(3, 1)));
    }

    #[test]
    fn history_pruning_cache_distinguishes_equal_origins_in_each_buffer() {
        let (tx, _rx) = crossbeam_channel::unbounded();
        let proxy = EventProxy::new(tx, Arc::new(|| {}));
        let term: SharedTerm = Arc::new(Mutex::new(Term::new(
            TermConfig::default(),
            &TermSize {
                columns: 4,
                screen_lines: 2,
            },
            proxy,
        )));
        let mut processor: Processor = Processor::new();
        {
            let mut term = term.lock().unwrap();
            processor.advance(&mut *term, b"\x1b[?1049h0\r\n1\r\n2\r\n3");
        }
        let origin = term.lock().unwrap().grid().history_origin();
        assert!(origin > 0, "alternate grid discarded at least one row");

        let images: Images = Arc::new(Mutex::new(vec![placement(0)]));
        let mut pruner = ImageHistoryPruner {
            // Simulate the numerically-equal primary cache entry that used to
            // suppress pruning after a screen transition.
            last_key: Some((false, origin)),
        };
        pruner.prune_if_changed(&term, &images);

        assert!(images.lock().unwrap().is_empty());
        assert_eq!(pruner.last_key, Some((true, origin)));
    }
}

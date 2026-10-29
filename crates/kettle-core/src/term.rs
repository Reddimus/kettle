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
    Config as TermConfig, GraphicsEvent, GraphicsEventBatch, GraphicsScrollDirection, MIN_COLUMNS,
};
use alacritty_terminal::vte::ansi::{
    Color as AnsiColor, CursorShape, Handler, NamedColor, Processor, SYNC_MARKER_CAPACITY,
};
use anyhow::{Context, Result};
use kettle_vt::kitty::{Delete as KittyDelete, DeleteTarget as KittyDeleteTarget, PlacementKey};
use kettle_vt::placeholder::{self, CellDiacritics, RawCell};
use kettle_vt::{
    Chunk, CompletionList, CompletionUpdate, DeferredGraphics, Extractor, PrivateOutputFilter,
    Progress, PromptKind,
};
use portable_pty::{CommandBuilder, PtySize};

use crate::event::{EventProxy, OutputWakeGate, TermEvent, Waker};
use crate::images::{
    AnimEntry, Animations, ImageSourceCrop, ImageSourceRect, Images, Placement, PlacementParams,
    RelEntry, RelativeResolver, Relatives, VirtualEntry, Virtuals, cascade_removed_relatives,
    prune, relative_origin,
};
use crate::persistence::{AsyncFileWriter, AsyncWriterStatus};
use crate::shell_startup::ShellStartup;

#[cfg(any(test, feature = "fuzzing"))]
pub(crate) mod fuzzing;

const PTY_READ_BUFFER_BYTES: usize = 64 * 1024;
const PTY_PUMP_QUEUE_DEPTH: usize = 4;
/// Pending [`Terminal::apply_local_output`] requests per pane.
const LOCAL_OUTPUT_QUEUE_DEPTH: usize = 4;
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
        if !self.canonical || self.extproc {
            return false;
        }
        let Some(byte) = self.veof else {
            return true;
        };
        !(self.istrip && byte >= 0x80
            || self.iuclc && self.iexten && byte.is_ascii_uppercase()
            || byte == b'\r' && (self.igncr || self.icrnl)
            || byte == b'\n'
            || self.ixon && [self.vstart, self.vstop].contains(&Some(byte))
            || self.isig && [self.vintr, self.vquit, self.vsusp].contains(&Some(byte))
            || [self.verase, self.vkill, self.veol].contains(&Some(byte))
            || self.iexten
                && [
                    self.vlnext,
                    self.vwerase,
                    self.vreprint,
                    self.vdiscard,
                    self.veol2,
                ]
                .contains(&Some(byte)))
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
mod pty_eof_tests;

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
        // Rust 1.99 renames fetch_update to try_update; the MSRV (1.95) predates it.
        #[allow(deprecated)]
        let _ = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                Some((state & !Self::STATUS_MASK) | status as u64)
            });
    }

    fn mark_chunk_read(&self) -> u64 {
        // Rust 1.99 renames fetch_update to try_update; the MSRV (1.95) predates it.
        #[allow(deprecated)]
        let previous = self
            .0
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |state| {
                let pending = (state & Self::PENDING_MASK) >> Self::PENDING_SHIFT;
                assert!(
                    pending < (1 << Self::PENDING_BITS) - 1,
                    "PTY pending-chunk counter overflowed"
                );
                Some(state.wrapping_add(Self::GENERATION_ONE + Self::PENDING_ONE))
            })
            .expect("PTY progress update is infallible");
        (previous >> Self::GENERATION_SHIFT).wrapping_add(1)
    }

    fn mark_chunk_handled(&self) {
        // Rust 1.99 renames fetch_update to try_update; the MSRV (1.95) predates it.
        #[allow(deprecated)]
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
            if result < 0 {
                let error = io::Error::last_os_error();
                if error.kind() == io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error);
            }
            let output =
                result > 0 && poll_fd.revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0;
            let child_exit = match unix_child_exit_code_unreaped(self.pid) {
                Ok(status) => status.is_some(),
                // This watcher owns a direct child's PID, but the Terminal's
                // status/diagnostic callers can reap it first. That is still
                // an exit observation: drain readable bytes and bound any
                // remaining slave holder just as for an unreaped exit.
                Err(error) if error.raw_os_error() == Some(libc::ECHILD) => true,
                Err(error) => return Err(error),
            };
            if output && child_exit {
                return Ok(Some(UnixStartupWake::OutputAndChildExit));
            }
            if output {
                return Ok(Some(UnixStartupWake::Output));
            }
            if child_exit {
                return Ok(Some(UnixStartupWake::ChildExit));
            }
            if deadline.is_some_and(|deadline| std::time::Instant::now() >= deadline) {
                return Ok(None);
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
        if std::time::Instant::now() >= deadline {
            return Ok(false);
        }
        let mut fd = libc::pollfd {
            fd: master_fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let result = unsafe { libc::poll(&mut fd, 1, poll_timeout_ms_until(deadline)) };
        if result == 0 || std::time::Instant::now() >= deadline {
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
    /// Whether the colour scheme is dark, for `CSI ? 996 n` and DEC mode
    /// 2031; `None` (headless) leaves both silent. Live changes go through
    /// [`Terminal::set_color_scheme`].
    pub color_scheme_dark: Option<bool>,
}

impl Default for TerminalCapabilities {
    fn default() -> Self {
        Self {
            osc52_copy: true,
            unnegotiated_modified_enter: true,
            contain_process_tree: false,
            observe_child_exit: false,
            color_scheme_dark: None,
        }
    }
}

/// Point `cmd` at the child's working directory under `policy`.
fn apply_working_directory(
    cmd: &mut CommandBuilder,
    cwd: Option<&std::path::Path>,
    cwd_policy: WorkingDirectoryPolicy,
) {
    match cwd {
        Some(d) if cwd_policy == WorkingDirectoryPolicy::RejectInvalidExplicit => {
            // Without `require_cwd` portable-pty replaces a directory that
            // vanished after the caller checked it with HOME.
            cmd.cwd(d);
            cmd.set_require_cwd(true);
        }
        None if cwd_policy == WorkingDirectoryPolicy::RejectInvalidExplicit => {}
        Some(d) if d.is_dir() => cmd.cwd(d),
        _ => {
            // The recorded cwd is missing or no longer on disk (e.g., the user
            // moved the repo between sessions, or the `-d` path has since been
            // deleted), so fall back to the OS home directory. `HOME` is unset
            // on Windows by default, so `home_dir_fallback` probes `HOME`, then
            // `USERPROFILE`, then `APPDATA`. A home that is not a directory
            // (such as `HOME=/etc/passwd`) would hand the OS spawn an invalid
            // target, so it counts as no home and `portable_pty` inherits
            // kettle's launch directory.
            if let Some(home) = home_dir_fallback(|k| std::env::var_os(k))
                && home.is_dir()
            {
                cmd.cwd(home);
            }
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
    /// Build a geometry, clamped to what the terminal engine can actually
    /// represent.
    ///
    /// Columns clamp to [`MIN_COLUMNS`], not to 1. The engine declares that
    /// minimum and then never enforces it, and a one-column grid is not merely
    /// cramped: writing any double-width character to it indexes past the end
    /// of the row and panics on the PTY reader thread. The release profile sets
    /// `panic = "abort"`, so that takes down every pane in every window, and a
    /// CJK filename in `ls` output is enough to trigger it.
    ///
    /// `kettle exec --cols 1`, a split narrow enough to leave one cell after
    /// padding, and the `(1, 1, 1, 1)` fallbacks for an unresolved geometry all
    /// ask for one column. Callers can bypass this constructor through the
    /// public fields, so [`TermSize::new`] clamps again at the engine boundary.
    pub fn new(columns: usize, rows: usize, pixel_width: u16, pixel_height: u16) -> Self {
        Self {
            columns: columns.max(MIN_COLUMNS),
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

/// How far a Kitty placement moves the cursor: right past the image and down
/// to its last row. A placement can ask for up to `u32::MAX` cells, but VT
/// parameters saturate at `u16::MAX`, and the terminal's line arithmetic
/// assumes they do, so both placement paths bound the counts here.
fn kitty_cursor_counts(cell_cols: usize, cell_rows: usize) -> (usize, usize) {
    let max = usize::from(u16::MAX);
    (cell_cols.min(max), cell_rows.saturating_sub(1).min(max))
}

fn kitty_cursor_movement(
    params: PlacementParams,
    cell_cols: usize,
    cell_rows: usize,
) -> Option<String> {
    if params.suppress_cursor_movement {
        return None;
    }
    let (cell_cols, down) = kitty_cursor_counts(cell_cols, cell_rows);
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

/// A `Write` sink that discards everything. On `Terminal` teardown the PTY
/// writer (the child's stdin / conin) is swapped for this, so the real writer
/// drops at once and the field never holds a dangling handle. Closing the
/// input is an EOF nudge for shells that exit when stdin closes, and it
/// synthesizes no terminal input. Zero-sized; never errors.
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

        if scrolls_into_history && start < 0.0 && end <= region_bottom {
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

fn send_graphics_reply(proxy: &EventProxy, reply: kettle_vt::kitty::GraphicsReply) {
    use alacritty_terminal::event::EventListener;
    proxy.send_event(TermEvent::PtyWrite(reply.encode()));
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
            | Chunk::ImageQuota(_)
            | Chunk::VirtualImage { .. }
            | Chunk::RelativePlacement { .. }
            | Chunk::Animation { .. }
            | Chunk::DeferredGraphics(_)
    )
}

/// How long a completion list stays on screen after a keystroke that is about
/// to refresh it. The card only has to survive the PTY round-trip of one Tab.
pub const COMPLETION_HIDE_GRACE: std::time::Duration = std::time::Duration::from_millis(200);
// Fish represents counters through its exact-integer `math` range. Staying at
// 2^52 - 1 gives every bundled adapter and Rust the same rollover point.
const MAX_COMPLETION_REQUEST: u64 = (1_u64 << 52) - 1;

#[derive(Default)]
struct CompletionSlot {
    /// One managed adapter may establish the terminal's first session even
    /// when its prompt/sync bytes were already in the reader pipeline as the
    /// window lost focus. There is no older session to resurrect at startup,
    /// and replies still need a post-focus Tab plus matching request number.
    /// The first accepted sync or ordinary editor input consumes this permit.
    startup_sync_pending: bool,
    /// Prompt session announced by a managed adapter. Every v3 reply must
    /// carry this value so a delayed control message cannot cross prompts.
    session: Option<u64>,
    generation: u64,
    list: Option<CompletionList>,
    /// Armed only by a Tab/Shift-Tab input. Unrelated input disarms publication
    /// before it enters the PTY, so a response already buffered by the shell
    /// cannot reopen a list for the command line that replaced it.
    accept_updates: bool,
    /// Managed shell adapters number every Tab/Shift-Tab request. Keeping the
    /// expected number here prevents output buffered for an earlier keypress
    /// from crossing an input or focus boundary.
    sequenced: bool,
    /// Raw completion keys the active shell keymap assigned to Kettle. A
    /// preserved custom binding must clear the card without consuming a
    /// request number that the shell adapter never advances.
    managed_keys: u8,
    /// A managed session may be established only immediately after OSC 133 A.
    /// Focus invalidation consumes this permission, so a buffered old `sync`
    /// cannot lift the quarantine when the window regains focus.
    sync_expected: bool,
    /// A command started after the last prompt. This distinguishes Fish's
    /// duplicate prompt mark on the same row from a genuinely new prompt that
    /// reused its row after a screen-clearing command.
    prompt_boundary_pending: bool,
    /// Ignore prompt resets from PTY chunks the pump had already admitted when
    /// focus was lost. The reader may process those chunks later, but they
    /// still belong to the pre-focus stream and cannot lift quarantine.
    ignore_sync_through_read: Option<u64>,
    next_request: u64,
    expected_request: Option<u64>,
    /// Tab requests admitted before the first managed `sync` identifies which
    /// bindings the shell actually owns. Keeping one count per direction lets
    /// the delayed sync reconcile Kettle's request number without counting a
    /// custom binding the adapter never ran.
    provisional_tab_requests: u64,
    provisional_backtab_requests: u64,
    /// Most recent pre-sync completion key since the last display boundary.
    /// The counts survive focus loss because the shell's request counter does;
    /// this arm does not, so a pre-focus reply cannot reopen the card.
    provisional_armed_key: Option<u8>,
    /// Cursor column paired with `anchor_input_prefix`. The pair outlives the
    /// visible card: focus changes, Ctrl-L, a shell-side clear, or the grace
    /// timeout can hide a list without changing the editor cycle that produced
    /// its prefix.
    anchor_cursor_col: Option<usize>,
    /// Stable input-prefix half of the card anchor. The outer option records
    /// whether a pair has ever been captured; the inner option preserves the
    /// protocol's "no safe prefix" value without confusing it with no pair.
    anchor_input_prefix: Option<Option<String>>,
    /// Cursor captured for the request currently in flight. If its reply
    /// carries a newly captured input prefix (for example, Fish continuing
    /// through a singleton directory), the two values become the next stable
    /// anchor pair. An ordinary cycle keeps the old pair.
    pending_cursor_col: Option<usize>,
    /// Legacy cooperative publishers have no request number. After focus loss
    /// they stay quarantined until the next prompt rather than risking a stale
    /// card; request-numbered adapters can resume immediately.
    legacy_quarantined: bool,
    /// Set when input that is expected to refresh the list was sent. The list
    /// stays visible until this passes, so a Tab that re-publishes the same
    /// candidates does not blink the card off for the round-trip.
    ///
    /// The shell's reply normally lands inside the window and clears the
    /// deadline. If a foreground program swallows the Tab instead, the event
    /// loop wakes at the deadline through [`Terminal::poll_completion_hide`]
    /// and draws one erase frame.
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum CompletionInputEffect {
    Ignore,
    Refresh(u8),
    Dismiss,
    Clear,
}

fn completion_input_effect(bytes: &[u8]) -> CompletionInputEffect {
    if bytes.is_empty() {
        return CompletionInputEffect::Ignore;
    }
    if bytes == b"\t" {
        return CompletionInputEffect::Refresh(kettle_vt::completion::KEY_TAB);
    }
    if bytes == b"\x1b[Z" {
        return CompletionInputEffect::Refresh(kettle_vt::completion::KEY_BACKTAB);
    }
    if bytes == b"\x0c" {
        // Ctrl-L redraws the prompt without changing the editor buffer in the
        // bundled shells. Hide the card but retain its cursor/prefix anchor so
        // a shell that keeps cycling the same result does not shift the card.
        return CompletionInputEffect::Dismiss;
    }
    let Some(parameters) = bytes
        .strip_prefix(b"\x1b[9")
        .and_then(|rest| rest.strip_suffix(b"u"))
    else {
        return CompletionInputEffect::Clear;
    };
    if parameters.is_empty() {
        return CompletionInputEffect::Refresh(kettle_vt::completion::KEY_TAB);
    }
    let Some(modifiers) = parameters.strip_prefix(b";") else {
        return CompletionInputEffect::Clear;
    };
    if modifiers.is_empty()
        || !modifiers
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b':' | b';'))
    {
        return CompletionInputEffect::Clear;
    }
    let mut key_fields = modifiers
        .split(|byte| *byte == b';')
        .next()
        .unwrap_or_default()
        .split(|byte| *byte == b':');
    let modifier = key_fields.next().unwrap_or_default();
    if modifier.is_empty() || !modifier.iter().all(u8::is_ascii_digit) {
        return CompletionInputEffect::Clear;
    }
    let event = key_fields.next();
    if key_fields.next().is_some() {
        return CompletionInputEffect::Clear;
    }
    let Some(modifier) = std::str::from_utf8(modifier)
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
    else {
        return CompletionInputEffect::Clear;
    };
    let Some(modifiers) = modifier.checked_sub(1) else {
        return CompletionInputEffect::Clear;
    };
    // Caps Lock and Num Lock do not change which completion direction the
    // shell sees. Every other modifier does: Fish and PSReadLine do not run
    // Kettle's stock Tab handlers for Alt/Ctrl/Super combinations, so counting
    // one here would advance only the terminal side of the request sequence.
    let key = match modifiers & !(64 | 128) {
        0 => kettle_vt::completion::KEY_TAB,
        1 => kettle_vt::completion::KEY_BACKTAB,
        _ => return CompletionInputEffect::Clear,
    };
    match event {
        Some(b"3") => CompletionInputEffect::Ignore,
        Some(b"1") | Some(b"2") | None => CompletionInputEffect::Refresh(key),
        _ => CompletionInputEffect::Clear,
    }
}

fn for_each_completion_input_effect(bytes: &[u8], mut visit: impl FnMut(CompletionInputEffect)) {
    if bytes.is_empty() {
        return;
    }
    // A bracketed paste is one editor insertion even when its payload contains
    // Tab bytes. Counting those as key bindings would advance the terminal's
    // request number while the shell deliberately does not. Paste admission
    // passes the complete bracketed frame as one input boundary; if that ever
    // becomes chunked, the caller must carry paste state across those chunks.
    if bytes.starts_with(b"\x1b[200~") && bytes.ends_with(b"\x1b[201~") {
        visit(CompletionInputEffect::Clear);
        return;
    }

    let mut index = 0usize;
    let mut pending_clear = false;
    while index < bytes.len() {
        let remaining = &bytes[index..];
        let special = if remaining.starts_with(b"\t") {
            Some((
                1,
                CompletionInputEffect::Refresh(kettle_vt::completion::KEY_TAB),
            ))
        } else if remaining.starts_with(b"\x1b[Z") {
            Some((
                3,
                CompletionInputEffect::Refresh(kettle_vt::completion::KEY_BACKTAB),
            ))
        } else if remaining.starts_with(b"\x0c") {
            Some((1, CompletionInputEffect::Dismiss))
        } else if remaining.starts_with(b"\x1b[9") {
            remaining
                .iter()
                .take(64)
                .position(|byte| *byte == b'u')
                .and_then(|end| {
                    let consumed = end + 1;
                    let effect = completion_input_effect(&remaining[..consumed]);
                    (effect != CompletionInputEffect::Clear).then_some((consumed, effect))
                })
        } else {
            None
        };

        let special = special.or_else(|| {
            if remaining.first() != Some(&0x1b) || remaining.len() < 2 {
                return None;
            }
            let consumed = if matches!(remaining[1], b'[' | b'O') {
                remaining
                    .iter()
                    .take(64)
                    .skip(2)
                    .position(|byte| (0x40..=0x7e).contains(byte))
                    .map_or(remaining.len().min(64), |end| end + 3)
            } else {
                2
            };
            Some((consumed, CompletionInputEffect::Clear))
        });
        if let Some((consumed, effect)) = special {
            if pending_clear {
                visit(CompletionInputEffect::Clear);
                pending_clear = false;
            }
            visit(effect);
            index += consumed;
        } else {
            pending_clear = true;
            index += 1;
        }
    }
    if pending_clear {
        visit(CompletionInputEffect::Clear);
    }
}

fn apply_completion_input_effect(
    slot: &mut CompletionSlot,
    effect: CompletionInputEffect,
    now: std::time::Instant,
    cursor_col: Option<usize>,
) {
    match effect {
        CompletionInputEffect::Ignore => {}
        CompletionInputEffect::Dismiss => dismiss_completion_slot(slot),
        CompletionInputEffect::Refresh(key) if !slot.sequenced || slot.managed_keys & key != 0 => {
            arm_completion_refresh_slot_at(slot, now, cursor_col, key)
        }
        CompletionInputEffect::Refresh(_) => clear_completion_slot(slot),
        CompletionInputEffect::Clear => {
            slot.startup_sync_pending = false;
            clear_completion_slot(slot);
        }
    }
}

#[cfg(test)]
fn with_completion_input_admission_slot<R>(
    slot: &mut CompletionSlot,
    inputs: &[&[u8]],
    now: std::time::Instant,
    admit: impl FnOnce() -> (R, bool),
) -> R {
    with_completion_input_admission_slot_at(slot, inputs, now, None, admit)
}

fn with_completion_input_admission_slot_at<R>(
    slot: &mut CompletionSlot,
    inputs: &[&[u8]],
    now: std::time::Instant,
    cursor_col: Option<usize>,
    admit: impl FnOnce() -> (R, bool),
) -> R {
    let (result, accepted) = admit();
    if accepted {
        let mut cursor_col = cursor_col;
        for input in inputs {
            for_each_completion_input_effect(input, |effect| {
                apply_completion_input_effect(slot, effect, now, cursor_col);
                if effect == CompletionInputEffect::Clear {
                    // Text or another editor mutation earlier in this admitted
                    // batch makes the pre-batch grid cursor stale. A later Tab
                    // may still open the card, but it must fall back to the
                    // grid edge instead of anchoring at a known-wrong column.
                    cursor_col = None;
                }
            });
        }
    }
    result
}

#[cfg(test)]
fn arm_completion_refresh_slot(slot: &mut CompletionSlot, now: std::time::Instant) {
    arm_completion_refresh_slot_at(slot, now, None, kettle_vt::completion::KEY_TAB);
}

fn arm_completion_refresh_slot_at(
    slot: &mut CompletionSlot,
    now: std::time::Instant,
    cursor_col: Option<usize>,
    key: u8,
) {
    slot.pending_cursor_col = cursor_col;
    if slot.sequenced {
        if slot.next_request >= MAX_COMPLETION_REQUEST {
            clear_completion_slot(slot);
            return;
        }
        slot.next_request += 1;
        slot.expected_request = Some(slot.next_request);
    } else {
        let total = slot
            .provisional_tab_requests
            .saturating_add(slot.provisional_backtab_requests);
        if total >= MAX_COMPLETION_REQUEST {
            clear_completion_slot(slot);
            slot.sync_expected = false;
            return;
        }
        match key {
            kettle_vt::completion::KEY_TAB => slot.provisional_tab_requests += 1,
            kettle_vt::completion::KEY_BACKTAB => slot.provisional_backtab_requests += 1,
            _ => {
                clear_completion_slot(slot);
                return;
            }
        }
        slot.provisional_armed_key = Some(key);
    }
    slot.accept_updates = true;
    if slot.list.is_some() {
        slot.hide_after = Some(now + COMPLETION_HIDE_GRACE);
    }
}

/// Hide only the card. Prompt redisplay can emit OSC 133;B while a managed
/// completion response is still crossing the PTY, so that visual boundary
/// must not retire the request which owns the response.
fn hide_completion_view_slot(slot: &mut CompletionSlot) {
    slot.list = None;
    slot.hide_after = None;
}

/// Hide current completion content and retire any in-flight response without
/// discarding the cursor/prefix pair. Several UI-only boundaries dismiss a
/// card while the shell intentionally retains its current completion cycle.
fn dismiss_completion_slot(slot: &mut CompletionSlot) {
    hide_completion_view_slot(slot);
    slot.accept_updates = false;
    slot.expected_request = None;
    slot.pending_cursor_col = None;
    slot.provisional_armed_key = None;
}

/// Retire completion content and the stable editor anchor. Use this only when
/// input or a command boundary can have changed the prefix itself.
fn clear_completion_slot(slot: &mut CompletionSlot) {
    dismiss_completion_slot(slot);
    slot.anchor_cursor_col = None;
    slot.anchor_input_prefix = None;
}

fn invalidate_completion_slot(slot: &mut CompletionSlot, read_generation: u64) {
    // The first managed sync is allowed to finish crossing the PTY boundary.
    // It carries no presentation permission by itself, and the provisional
    // request counts below still reject every response older than the first
    // post-focus Tab. Once a session has been trusted, keep the established
    // stricter rule: a buffered sync can never replace it after focus loss.
    let initial_sync_pending =
        slot.startup_sync_pending && !slot.sequenced && slot.session.is_none();
    dismiss_completion_slot(slot);
    slot.legacy_quarantined = true;
    slot.sync_expected = initial_sync_pending;
    slot.startup_sync_pending = initial_sync_pending;
    slot.ignore_sync_through_read = Some(read_generation);
}

fn apply_completion_update(cell: &Mutex<CompletionSlot>, update: CompletionUpdate) {
    let Ok(mut current) = cell.lock() else {
        return;
    };
    match update {
        CompletionUpdate::Sync { session, keys } if current.sync_expected => {
            let next_request = current
                .provisional_tab_requests
                .checked_mul(u64::from(keys & kettle_vt::completion::KEY_TAB != 0))
                .and_then(|count| {
                    current
                        .provisional_backtab_requests
                        .checked_mul(u64::from(keys & kettle_vt::completion::KEY_BACKTAB != 0))
                        .and_then(|reverse| count.checked_add(reverse))
                })
                .filter(|count| *count <= MAX_COMPLETION_REQUEST)
                .unwrap_or(MAX_COMPLETION_REQUEST);
            let request_armed = current.accept_updates
                && current
                    .provisional_armed_key
                    .is_some_and(|key| keys & key != 0)
                && next_request > 0;
            let pending_cursor_col = request_armed
                .then_some(current.pending_cursor_col)
                .flatten();
            *current = CompletionSlot {
                session: Some(session),
                sequenced: true,
                managed_keys: keys,
                next_request,
                expected_request: request_armed.then_some(next_request),
                accept_updates: request_armed,
                pending_cursor_col,
                ..CompletionSlot::default()
            };
        }
        CompletionUpdate::Keymap { session, keys }
            if current.sequenced && current.session == Some(session) =>
        {
            dismiss_completion_slot(&mut current);
            current.managed_keys = keys;
        }
        CompletionUpdate::Show(list) | CompletionUpdate::Update(list)
            if current.accept_updates
                && list.generation >= current.generation
                && match (list.session, list.request) {
                    (Some(session), Some(request)) => {
                        current.sequenced
                            && current.session == Some(session)
                            && current.expected_request == Some(request)
                    }
                    (None, None) => !current.sequenced && !current.legacy_quarantined,
                    _ => false,
                } =>
        {
            let prefix_changed = current.anchor_input_prefix.as_ref() != Some(&list.input_prefix);
            if prefix_changed {
                current.anchor_cursor_col = current.pending_cursor_col;
                current.anchor_input_prefix = Some(list.input_prefix.clone());
            }
            current.pending_cursor_col = None;
            current.generation = list.generation;
            current.list = Some(list);
            // The refresh the grace window was waiting for arrived.
            current.hide_after = None;
        }
        CompletionUpdate::Clear {
            session,
            generation,
            request,
        } if generation >= current.generation
            && match (session, request) {
                (Some(session), Some(request)) => {
                    current.sequenced
                        && current.session == Some(session)
                        && current.expected_request == Some(request)
                }
                (None, None) => !current.sequenced && !current.legacy_quarantined,
                _ => false,
            } =>
        {
            current.generation = generation;
            dismiss_completion_slot(&mut current);
        }
        CompletionUpdate::Sync { .. }
        | CompletionUpdate::Keymap { .. }
        | CompletionUpdate::Show(_)
        | CompletionUpdate::Update(_)
        | CompletionUpdate::Clear { .. } => {}
    }
}

fn reset_completion_session(cell: &Mutex<CompletionSlot>, read_generation: u64) {
    if let Ok(mut current) = cell.lock() {
        if current
            .ignore_sync_through_read
            .is_some_and(|boundary| read_generation <= boundary)
        {
            let startup_sync_pending = current.startup_sync_pending;
            clear_completion_slot(&mut current);
            current.sync_expected = startup_sync_pending;
            return;
        }
        let startup_sync_pending = current.startup_sync_pending;
        *current = CompletionSlot {
            startup_sync_pending,
            sync_expected: true,
            ..CompletionSlot::default()
        };
    }
}

/// A new prompt row without an intervening command can be either a redraw that
/// emits no managed sync (Fish Ctrl-L) or a shell reset that does (PowerShell
/// Ctrl-C). Preserve the current session so the former keeps working, while
/// opening one bounded sync window so the latter can replace it.
fn prepare_optional_completion_resync(cell: &Mutex<CompletionSlot>, read_generation: u64) {
    if let Ok(mut current) = cell.lock() {
        if current
            .ignore_sync_through_read
            .is_some_and(|boundary| read_generation <= boundary)
        {
            let startup_sync_pending = current.startup_sync_pending;
            clear_completion_slot(&mut current);
            current.sync_expected = startup_sync_pending;
            return;
        }
        dismiss_completion_slot(&mut current);
        current.legacy_quarantined = false;
        current.sync_expected = true;
        current.prompt_boundary_pending = false;
    }
}

fn apply_completion_prompt_start(
    cell: &Mutex<CompletionSlot>,
    read_generation: u64,
    prompt_mark_was_new: bool,
) {
    let command_started = cell
        .lock()
        .map(|slot| slot.prompt_boundary_pending)
        .unwrap_or(true);
    if command_started {
        reset_completion_session(cell, read_generation);
    } else if prompt_mark_was_new {
        prepare_optional_completion_resync(cell, read_generation);
    }
}

/// Reader-thread bridge from one parsed prompt mark to both retained prompt
/// navigation and completion-session state. Keeping the two decisions in one
/// function prevents a future reader refactor from deduplicating the prompt
/// ring but unconditionally resetting completion again.
fn apply_completion_reader_prompt_start(
    completion: &Mutex<CompletionSlot>,
    prompts: &Mutex<std::collections::VecDeque<u64>>,
    prompt_row: Option<(u64, u64)>,
    read_generation: u64,
) {
    let Some((row_id, history_origin)) = prompt_row else {
        // Alternate-screen prompt marks and poisoned terminal/prompt locks are
        // not safe redraws. Fail closed instead of preserving a session whose
        // screen identity could not be established.
        reset_completion_session(completion, read_generation);
        return;
    };
    let starts_new_session = {
        if let Ok(mut marks) = prompts.lock() {
            marks.retain(|mark| *mark >= history_origin);
            push_prompt_mark(&mut marks, row_id)
        } else {
            true
        }
    };
    apply_completion_prompt_start(completion, read_generation, starts_new_session);
}

#[cfg(test)]
mod completion_state_tests;

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
    let mut replay_failed = false;
    extractor.feed_with(graphics.as_bytes(), |extractor, chunk| {
        if replay_failed {
            return;
        }
        match chunk {
            Chunk::Pass(_) | Chunk::Terminal(_) | Chunk::Raw(_) => {
                // The downstream text engine intentionally ignores malformed
                // or incomplete graphics controls. The extractor still had to
                // replay them to update bounded partial-upload state.
            }
            chunk => {
                if !apply_graphics_chunk_at(term, chunk, actions, extractor) {
                    replay_failed = true;
                }
            }
        }
    });
    extractor.set_graphics_deferred(true);
    if replay_failed {
        reset_deferred_graphics(active_alternate, deferred, registries, extractor);
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

/// Borrowed state for [`force_sync_update_flush`], which force-applies a
/// pending DEC 2026 update, prunes image rows evicted by the buffered
/// mutation, then publishes exactly one redraw.
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
    raw_tx: &crossbeam_channel::Sender<Option<(u64, Vec<u8>, usize)>>,
    drain_output: &AtomicBool,
    generation: u64,
    mut buffer: Vec<u8>,
    read_len: usize,
) -> PtyPumpSend {
    loop {
        if drain_output.load(Ordering::Acquire) {
            return PtyPumpSend::Drain(buffer);
        }

        match raw_tx.send_timeout(
            Some((generation, buffer, read_len)),
            std::time::Duration::from_millis(10),
        ) {
            Ok(()) => return PtyPumpSend::Forwarded,
            Err(crossbeam_channel::SendTimeoutError::Timeout(Some((_, returned, _)))) => {
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

/// One unit of parser work.
enum ParserInput {
    /// A pump read: source generation, buffer, and bytes read.
    Pty(u64, Vec<u8>, usize),
    /// Bytes Kettle parses without the child writing them. See
    /// [`Terminal::apply_local_output`].
    Local(Vec<u8>),
}

/// A local output request: the pump's read generation when it was made, and
/// the bytes.
type LocalOutput = (u64, Vec<u8>);

/// Receive the next pump chunk or local output while enforcing the DEC 2026
/// deadline ahead of ready data, and flushing immediately when EOF makes a
/// terminator impossible. `None` means the pump has finished.
///
/// A local request waits in `pending_local` until the reader has handled
/// every PTY read that finished before it was made (`handled_generation`), so
/// output the terminal had already read cannot land after a Reset. At most
/// the queued reads come first, so a flood cannot hold it back.
fn receive_parser_input(
    processor: &mut Processor,
    raw_rx: &crossbeam_channel::Receiver<Option<(u64, Vec<u8>, usize)>>,
    local_rx: &mut crossbeam_channel::Receiver<LocalOutput>,
    pending_local: &mut Option<LocalOutput>,
    handled_generation: u64,
    context: &mut SyncFlushContext<'_>,
) -> Option<ParserInput> {
    enum Received {
        Pty(Result<Option<(u64, Vec<u8>, usize)>, crossbeam_channel::RecvError>),
        Local(Result<LocalOutput, crossbeam_channel::RecvError>),
        Timeout,
    }

    loop {
        let deadline = processor.sync_timeout().sync_timeout();
        let now = std::time::Instant::now();
        if deadline.is_some_and(|deadline| now >= deadline) {
            force_sync_update_flush(processor, context);
            continue;
        }
        if pending_local
            .as_ref()
            .is_some_and(|(generation, _)| *generation <= handled_generation)
        {
            let (_, bytes) = pending_local.take().expect("checked above");
            return Some(ParserInput::Local(bytes));
        }

        // While a request waits for earlier reads, take only PTY chunks.
        let waiting = crossbeam_channel::never();
        let local_source = if pending_local.is_some() {
            &waiting
        } else {
            &*local_rx
        };
        // Try both without blocking first. A flood keeps the PTY queue
        // non-empty, and `select!` costs more per chunk than a plain receive.
        let ready = match local_source.try_recv() {
            Ok(request) => Some(Received::Local(Ok(request))),
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                Some(Received::Local(Err(crossbeam_channel::RecvError)))
            }
            Err(crossbeam_channel::TryRecvError::Empty) => match raw_rx.try_recv() {
                Ok(chunk) => Some(Received::Pty(Ok(chunk))),
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    Some(Received::Pty(Err(crossbeam_channel::RecvError)))
                }
                Err(crossbeam_channel::TryRecvError::Empty) => None,
            },
        };
        let received = match (ready, deadline) {
            (Some(ready), _) => ready,
            (None, Some(deadline)) => crossbeam_channel::select! {
                recv(raw_rx) -> chunk => Received::Pty(chunk),
                recv(local_source) -> request => Received::Local(request),
                default(deadline.saturating_duration_since(now)) => Received::Timeout,
            },
            (None, None) => crossbeam_channel::select! {
                recv(raw_rx) -> chunk => Received::Pty(chunk),
                recv(local_source) -> request => Received::Local(request),
            },
        };
        match received {
            Received::Pty(Ok(chunk)) => {
                // A ready chunk and the timeout can race at the wait boundary.
                // The expired synchronized update takes priority so sustained
                // output cannot starve its flush. EOF also proves no closing
                // sequence can still arrive.
                if deadline.is_some_and(|deadline| {
                    chunk.is_none() || std::time::Instant::now() >= deadline
                }) {
                    force_sync_update_flush(processor, context);
                }
                return chunk
                    .map(|(generation, buffer, n)| ParserInput::Pty(generation, buffer, n));
            }
            Received::Pty(Err(crossbeam_channel::RecvError)) => {
                if deadline.is_some() {
                    force_sync_update_flush(processor, context);
                }
                return None;
            }
            // Released at the top of the loop once its earlier reads are in.
            Received::Local(Ok(request)) => *pending_local = Some(request),
            // The `Terminal` is gone. A closed channel is always ready, so stop
            // selecting on it and keep draining the child's output.
            Received::Local(Err(crossbeam_channel::RecvError)) => {
                *local_rx = crossbeam_channel::never();
            }
            Received::Timeout => force_sync_update_flush(processor, context),
        }
    }
}

/// The pump-only form of [`receive_parser_input`] used by tests that
/// exercise the DEC 2026 deadline.
#[cfg(test)]
fn receive_pty_chunk(
    processor: &mut Processor,
    raw_rx: &crossbeam_channel::Receiver<Option<(u64, Vec<u8>, usize)>>,
    context: &mut SyncFlushContext<'_>,
) -> Option<(u64, Vec<u8>, usize)> {
    let mut local_rx = crossbeam_channel::never();
    match receive_parser_input(processor, raw_rx, &mut local_rx, &mut None, 0, context)? {
        ParserInput::Pty(generation, buffer, n) => Some((generation, buffer, n)),
        ParserInput::Local(_) => unreachable!("the local channel never delivers"),
    }
}

/// What a pane's terminal says about who should get a key that both Kettle
/// and a program in the pane could use: Kettle's split resize on Shift+Arrow,
/// say, against Codex's Shift+Left. Kettle-ui decides from this snapshot and
/// its own state (splits, tabs, selection); see its program-keyboard rules.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct KeyboardClaims {
    /// The active screen negotiated kitty keyboard flags or xterm
    /// modifyOtherKeys. A program asks for these, and so does a fish prompt.
    pub keyboard_protocol: bool,
    /// Mouse reporting is on (fzf, htop, Neovim with `mouse=a`). A shell
    /// prompt never turns it on.
    pub mouse_mode: bool,
    pub alt_screen: bool,
    /// Scrollback rows above the screen, and how far the view is scrolled
    /// into them.
    pub history_size: usize,
    pub display_offset: usize,
    /// A retained OSC 133 prompt lies above / below the current view, so
    /// jumping to a prompt would move.
    pub prompt_above: bool,
    pub prompt_below: bool,
    /// Shell integration shows the shell at its prompt: a prompt mark has
    /// been seen and no command has started since (see
    /// [`Terminal::shell_at_prompt`]).
    pub at_prompt: bool,
}

impl KeyboardClaims {
    /// A program, not a shell sitting at its prompt, owns the keyboard. The
    /// alternate screen and mouse reporting always mean a program, even one a
    /// shell key binding started without a command mark (fzf on Ctrl+T). A
    /// keyboard protocol means a program only away from the prompt, since
    /// fish pushes kitty flags for its own line editor.
    pub fn program_owns_keyboard(&self) -> bool {
        self.alt_screen || self.mouse_mode || (self.keyboard_protocol && !self.at_prompt)
    }
}

/// What XTVERSION (`CSI > q`) reports: the name and version that programs
/// such as Claude Code and tmux key their capability probes on.
pub(crate) fn xtversion() -> String {
    format!("kettle({})", env!("CARGO_PKG_VERSION"))
}

/// Grid dimensions passed to `alacritty_terminal` (implements `Dimensions`).
#[derive(Clone, Copy)]
pub struct TermSize {
    pub columns: usize,
    pub screen_lines: usize,
}

impl TermSize {
    /// Build a size the engine can actually represent.
    ///
    /// The second guard on the one-column crash. [`PtyGeometry::new`] clamps
    /// too, but its fields are public and the type is re-exported, so a caller
    /// can build `PtyGeometry { columns: 1, .. }` and never pass through it.
    /// The engine consumes this type directly, so clamping here holds however
    /// the geometry was made.
    pub fn new(columns: usize, screen_lines: usize) -> Self {
        Self {
            columns: columns.max(MIN_COLUMNS),
            screen_lines: screen_lines.max(1),
        }
    }
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

#[cfg(test)]
fn commit_local_geometry(
    term: &SharedTerm,
    geometry: &Arc<Mutex<VersionedPtyGeometry>>,
    desired: PtyGeometry,
    grid_options: Option<TermConfig>,
) {
    commit_local_geometry_inner(term, geometry, None, desired, grid_options);
}

fn commit_local_geometry_inner(
    term: &SharedTerm,
    geometry: &Arc<Mutex<VersionedPtyGeometry>>,
    prompts: Option<&Mutex<std::collections::VecDeque<u64>>>,
    desired: PtyGeometry,
    grid_options: Option<TermConfig>,
) {
    // This lock order is an invariant shared with image placement and geometry
    // snapshots: Term first, geometry second.
    let mut term = term.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let old_columns = term.columns();
    let old_cursor = {
        let grid = term.grid();
        stable_grid_line_id(
            grid.history_origin(),
            grid.history_size(),
            grid.cursor.point.line.0,
        )
    };
    let grid_resized = grid_options.is_some();
    if let Some(options) = grid_options {
        term.set_options_preserving_history(options);
        term.resize(TermSize::new(desired.columns, desired.rows));
    }
    {
        let mut geometry = geometry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        geometry.geometry = desired;
        geometry.generation = geometry.generation.wrapping_add(1);
    }
    if grid_resized
        && old_columns != desired.columns
        && let Some(prompts) = prompts
        && let Ok(mut marks) = prompts.lock()
    {
        let grid = term.grid();
        let new_cursor = stable_grid_line_id(
            grid.history_origin(),
            grid.history_size(),
            grid.cursor.point.line.0,
        );
        rebase_prompt_marks_after_column_reflow(
            &mut marks,
            old_columns,
            desired.columns,
            old_cursor,
            new_cursor,
        );
    }
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
/// An *empty* env var (e.g., `HOME=""`, possible in stripped-down CI
/// containers or after a misconfigured shell `unset HOME` / `export
/// HOME=`) is treated as unset and the probe continues to the next
/// variable. Returning `PathBuf::from("")` would hand
/// `CommandBuilder::cwd` an invalid empty path (on Unix that means
/// "no cwd") instead of picking a home.
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
    pub generation: u64,
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
    #[cfg(unix)]
    startup_geometry: Option<crate::startup_geometry::Trace>,
    /// State shared with an asynchronous ConPTY close. Once the master moves
    /// to that worker, only it may publish `stop`: on older Windows releases
    /// `ClosePseudoConsole` can wait for the reader to drain conout, so a later
    /// `Terminal::drop` must not stop that reader first.
    #[cfg(windows)]
    pty_close: Option<Arc<ConPtyCloseState>>,
    writer: Arc<Mutex<Box<dyn Write + Send>>>,
    /// Requests for [`Terminal::apply_local_output`], parsed by the reader.
    local_output: crossbeam_channel::Sender<LocalOutput>,
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
    command_generation: Arc<std::sync::atomic::AtomicU64>,
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
    /// The shell's working directory, read from the OS process table,
    /// for shells that do NOT emit OSC 7/9;9, such as a stock Windows `cmd`.
    /// Kept SEPARATE from `cwd` so a stale/None native read can never clobber
    /// the authoritative escape-sequence cwd; consulted only as a fallback by
    /// `current_dir_or_native`. Never set for WSL/SSH panes (the relay's OS cwd
    /// is meaningless there).
    pub native_cwd: Arc<Mutex<Option<String>>>,
    /// Set once the shell actually reported a cwd via OSC 7/9;9. Until
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
    /// True once this pane has seen at least one OSC 133 OutputStart (C). An
    /// integration that emits A/B/D but never C (a clobbered pwsh Enter handler,
    /// AcceptLine via other chords) must never make `shell_idle` report "idle".
    /// Prompt marks alone don't prove command tracking works, and a false idle
    /// skips the close-confirm dialog over a running command.
    output_start_seen: Arc<std::sync::atomic::AtomicBool>,
    /// Mirrors an active session-log writer so the reader thread can skip the
    /// Mutex and `Option` check on each 64KiB read when logging is off, the
    /// common case. Toggled ONLY through [`Terminal::set_log_path`] or
    /// [`Terminal::set_log_file`], which keeps the pair in sync.
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
    /// State events (bell, title, cursor blinking, OSC 52 writes, exit) the
    /// event proxy latched since the owner last took them.
    latched: Arc<crate::event::LatchedEvents>,
    /// Live OSC 52 write policy shared with the terminal event proxy. DA1
    /// extension 52 is emitted only while this is true.
    osc52_copy_allowed: Arc<AtomicBool>,
    /// Bumped by the reader thread once per PTY read it processed (right
    /// before the wakeup fires). Lets a UI hosting several windows answer "did
    /// THIS pane produce output since I last painted?" without draining
    /// anything, so a fan-out wakeup repaints only the windows whose panes'
    /// generations moved. Plain text emits no `TermEvent`, so the event channel
    /// can't answer that question.
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

fn command_admission_generation(
    started: &Mutex<Option<std::time::Instant>>,
    generation: &std::sync::atomic::AtomicU64,
) -> Option<u64> {
    // The reader publishes the generation and running state under this lock.
    let started = started.lock().ok()?;
    if started.is_some() {
        return None;
    }
    Some(generation.load(std::sync::atomic::Ordering::Relaxed))
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
mod shell_activity_tests;

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

#[cfg(all(test, unix))]
mod local_output_tests {
    use super::*;

    /// `/bin/cat` in cooked mode: the line discipline echoes every byte the
    /// child receives, so input that reaches the child shows up on screen.
    fn cat_terminal() -> Option<(Terminal, crossbeam_channel::Receiver<TermEvent>)> {
        spawn_terminal(&["/bin/cat".to_string()])
    }

    fn spawn_terminal(
        argv: &[String],
    ) -> Option<(Terminal, crossbeam_channel::Receiver<TermEvent>)> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let waker: Waker = Arc::new(|| {});
        match Terminal::new(
            argv,
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
            Ok(terminal) => Some((terminal, rx)),
            Err(error) => {
                eprintln!("skipping local output test: no PTY ({error})");
                None
            }
        }
    }

    fn wait_for(
        terminal: &Terminal,
        events: &crossbeam_channel::Receiver<TermEvent>,
        done: impl Fn(&Terminal, &str) -> bool,
    ) -> bool {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            while let Ok(event) = events.try_recv() {
                if let TermEvent::PtyWrite(reply) = event {
                    terminal.write(reply.as_bytes());
                }
            }
            if let Some(screen) = terminal.screen_text(0)
                && done(terminal, &screen.text)
            {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        false
    }

    #[test]
    fn virtual_and_relative_placements_use_the_selected_animation_frame() {
        let script = concat!(
            r"printf '\033_Ga=T,i=7,p=1,q=2,f=24,s=1,v=1,C=1;AAAA\033\\",
            r"\033_Ga=p,i=7,p=2,q=2,U=1,c=1,r=1\033\\",
            r"\033_Ga=p,i=7,p=3,q=2,P=7,Q=1,H=1,V=1\033\\",
            r"\033_Ga=f,i=7,q=2,f=24,s=1,v=1;/wAA\033\\",
            r"\033_Ga=a,i=7,q=2,c=2,s=1\033\\",
            r"\033[38;5;7m\364\216\273\256\033[0mREADY'; exec sleep 10",
        );
        let argv = ["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
        let (terminal, events) = spawn_terminal(&argv).expect("test PTY");
        assert!(wait_for(&terminal, &events, |_, text| text.contains("READY")));

        let physical = terminal.placements();
        let tiles = terminal.placeholder_tiles();
        let relatives = terminal.relative_tiles();
        assert_eq!(physical.len(), 1);
        assert_eq!(tiles.len(), 1);
        assert_eq!(relatives.len(), 1);
        let selected = terminal.anims.lock().unwrap()[&7].imgs[1].clone();
        for placement in [&physical[0], &tiles[0], &relatives[0]] {
            assert_eq!(placement.img.rgba.as_slice(), &[255, 0, 0, 255]);
            assert_eq!(placement.img.allocation_key(), selected.allocation_key());
        }
        assert_eq!(
            tiles[0].source_rect,
            Some(ImageSourceRect {
                x: 0,
                y: 0,
                width: 1,
                height: 1,
            })
        );
    }

    #[test]
    fn a_local_reset_clears_the_terminal_and_never_reaches_the_child() {
        let Some((terminal, events)) = cat_terminal() else {
            return;
        };
        terminal.write(b"before\n");
        // The line discipline echoes the line and `cat` prints it again. Wait
        // for both, so neither arrives after the reset.
        assert!(wait_for(&terminal, &events, |_, text| {
            text.matches("before").count() == 2
        }));

        assert!(terminal.apply_local_output(b"\x1bc"));
        assert!(
            wait_for(&terminal, &events, |_, text| !text.contains("before")),
            "RIS did not reset the terminal"
        );

        // Anything the child received would be echoed before this line.
        terminal.write(b"after\n");
        assert!(wait_for(&terminal, &events, |_, text| text.contains("after")));
        let text = terminal.screen_text(0).expect("screen").text;
        assert!(
            !text.contains("^["),
            "the reset reached the child: {text:?}"
        );
    }

    /// The extractor holds a private completion OSC until its terminator and
    /// counts `ESC` followers as payload. A local reset must not wait behind
    /// a string the child left open, and must not print its payload.
    #[test]
    fn a_local_reset_applies_while_the_child_holds_a_private_string_open() {
        let script = r"printf 'before\n\033]777;kettle-completion;partial'; exec sleep 10";
        let argv = ["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
        let Some((terminal, events)) = spawn_terminal(&argv) else {
            return;
        };
        assert!(wait_for(&terminal, &events, |_, text| text.contains("before")));

        assert!(terminal.apply_local_output(b"\x1bc"));
        assert!(
            wait_for(&terminal, &events, |_, text| !text.contains("before")),
            "the unfinished private string swallowed the reset"
        );
        let text = terminal.screen_text(0).expect("screen").text;
        assert!(
            !text.contains("partial"),
            "the private payload leaked: {text:?}"
        );
    }

    /// Prints 40 lines and then a marker with no newline, and waits in `cat`.
    /// One process writes all of it, so once the marker is on screen no
    /// output is still arriving. Lines typed into `cat` would come back
    /// twice, as the line discipline's echo and as `cat`'s copy, and Linux can
    /// interleave the two mid-line, so no count of them marks the end.
    const LINES_THEN_CAT: &str = r#"i=0; while [ $i -lt 40 ]; do echo "line $i"; i=$((i+1)); done; printf lines-done; exec cat"#;

    #[test]
    fn a_local_history_clear_keeps_the_screen() {
        let argv = [
            "/bin/sh".to_string(),
            "-c".to_string(),
            LINES_THEN_CAT.to_string(),
        ];
        let Some((terminal, events)) = spawn_terminal(&argv) else {
            return;
        };
        let history = |terminal: &Terminal| terminal.term.lock().unwrap().grid().history_size();
        assert!(
            wait_for(&terminal, &events, |terminal, text| {
                text.contains("lines-done") && history(terminal) > 0
            }),
            "the lines never arrived: {:?}",
            terminal.screen_text(100).map(|screen| screen.text)
        );
        let before = terminal.screen_text(0).expect("screen").text;

        assert!(terminal.apply_local_output(b"\x1b[3J"));
        assert!(
            wait_for(&terminal, &events, |terminal, _| history(terminal) == 0),
            "CSI 3 J did not clear the scrollback"
        );
        assert_eq!(
            terminal.screen_text(0).expect("screen").text,
            before,
            "the visible screen changed"
        );

        // Anything the child received would be echoed before this line.
        terminal.write(b"after\n");
        assert!(wait_for(&terminal, &events, |_, text| text.contains("after")));
        let text = terminal.screen_text(0).expect("screen").text;
        assert!(
            !text.contains("^["),
            "the clear reached the child: {text:?}"
        );
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
/// Reader and writer setup finishes before `spawn_command`, so a setup error
/// cannot start a child at all. Dropping a `Box<dyn Child>` does not terminate
/// the process it represents, so the guard covers the final ownership handoff
/// and protects an unwind while the value is assembled.
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
/// because `Path` only treats `\` as a separator on Windows targets. On a
/// Linux/macOS build (incl. CI) `C:\Windows\System32\wsl.exe` would be one
/// opaque component and the stem check would miss it. wsl.exe only runs on
/// Windows, but a target-independent check keeps the function and its unit
/// test correct everywhere.
fn is_wsl_launcher(prog: &str) -> bool {
    let last = prog.rsplit(['/', '\\']).next().unwrap_or(prog);
    last.eq_ignore_ascii_case("wsl") || last.eq_ignore_ascii_case("wsl.exe")
}

/// The command a pane runs: `argv`, or the user's shell when it is empty,
/// with Kettle's PowerShell integration where it applies. `login_shell` adds
/// `-l` to an explicit POSIX shell. The user's shell needs no `-l`: on Unix
/// it already starts as a login shell, with `-` and its name as `argv[0]`,
/// and takes no arguments of its own; on Windows it is pwsh, powershell or
/// cmd, none of which take `-l` as a login switch.
fn pane_command(argv: &[String], login_shell: bool, shell_integration: bool) -> CommandBuilder {
    match argv.split_first() {
        Some((prog, rest)) => {
            explicit_prog_with_integration(prog, rest, login_shell, shell_integration)
        }
        None => default_prog_with_integration(shell_integration),
    }
}

/// Whether an EXPLICIT `command = <prog>` accepts the POSIX `-l` login switch.
///
/// Returns `false` for `wsl.exe`, where `-l` means "list distros", and for the
/// Windows-native shells (`pwsh`/`powershell`/`cmd`), which don't treat `-l`
/// as a login flag. Matches on the case-insensitive basename sans `.exe`.
/// POSIX shells (bash/zsh/fish/…) and anything else honor `-l`.
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
    // Dropdown parity: Windows Terminal's order and its "PowerShell" label
    // for pwsh 7. The order matters beyond looks: `Ctrl+Shift+N` opens the
    // Nth entry, matching WT's profile shortcuts.
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

#[cfg(any(windows, test))]
mod shell_probe;

/// Dropdown parity: probe the newest VS via
/// `%ProgramFiles(x86)%\Microsoft Visual Studio\Installer\vswhere.exe`
/// (a fixed location Microsoft documents as stable). Like `list_wsl_distros`,
/// capture is bounded and a timed-out probe is killed and reaped.
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
    let mut command = std::process::Command::new(vswhere);
    command.args([
        "-latest",
        "-products",
        "*",
        "-requires",
        "Microsoft.VisualStudio.Component.CoreEditor",
        "-property",
        "installationPath",
    ]);
    let out = shell_probe::output(command, std::time::Duration::from_secs(2), 64 * 1024).ok()?;
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
    let mut command = std::process::Command::new("wsl.exe");
    command.args(["-l", "-q"]);
    let Ok(out) = shell_probe::output(command, std::time::Duration::from_secs(2), 64 * 1024) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    let units: Vec<u16> = out
        .stdout
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    parse_wsl_distros(&String::from_utf16_lossy(&units))
}

/// One axis of a `PtySize`, computed without overflow. `cell` is the
/// per-cell pixel extent (1 when computing the row/column count itself);
/// `count` is the grid dimension in cells. The product is evaluated in
/// `u32` and clamped into `u16`, the type `PtySize` requires. A `u16`
/// multiply panics in debug and wraps in release once the product passes
/// 65535, which a HiDPI cell on a very wide grid can reach, and casting
/// `count` to `u16` first would truncate a pathological `usize`.
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

/// The shell a pane started with no command runs, `shell` being the
/// `SHELL` its configured environment sets, if any. As the PTY picks it:
/// its `SHELL` (that one, else Kettle's own, else the user's passwd shell)
/// if it can run, else the passwd shell, else `/bin/sh`.
#[cfg(unix)]
pub fn default_pane_shell(shell: Option<&std::ffi::OsStr>) -> String {
    let mut cmd = default_prog();
    if let Some(shell) = shell {
        cmd.env("SHELL", shell);
    }
    cmd.get_shell()
}

#[cfg(all(test, unix))]
mod default_pane_shell_tests {
    use super::default_pane_shell;
    use std::os::unix::fs::PermissionsExt as _;

    /// A configured `SHELL` that can run is the one; one that cannot is
    /// passed over for a shell that can, as the PTY passes it over.
    #[test]
    fn a_default_pane_runs_the_shell_the_pty_picks() {
        let dir = kettle_test_support::private_tempdir("kettle-pane-shell-");
        let runnable = dir.path().join("bash");
        std::fs::write(&runnable, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&runnable, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            default_pane_shell(Some(runnable.as_os_str())),
            runnable.to_str().unwrap()
        );
        let unrunnable = dir.path().join("zsh");
        std::fs::write(&unrunnable, "").unwrap();
        let picked = default_pane_shell(Some(unrunnable.as_os_str()));
        assert_ne!(picked, unrunnable.to_str().unwrap());
        let mode = std::fs::metadata(&picked).unwrap().permissions().mode();
        assert_ne!(mode & 0o111, 0, "{picked}");
        assert!(!default_pane_shell(None).is_empty());
    }
}

/// The default-shell `CommandBuilder`, optionally auto-injecting
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

#[cfg(any(windows, test))]
fn is_powershell_program(program: &str) -> bool {
    let base = program
        .rsplit(['/', '\\'])
        .next()
        .unwrap_or(program)
        .to_ascii_lowercase();
    matches!(
        base.as_str(),
        "pwsh.exe" | "powershell.exe" | "pwsh" | "powershell"
    )
}

#[cfg(any(windows, test))]
fn should_inject_explicit_powershell(program: &str, arguments: &[String], inject: bool) -> bool {
    inject && arguments.is_empty() && is_powershell_program(program)
}

fn explicit_prog_with_integration(
    prog: &str,
    rest: &[String],
    login_shell: bool,
    shell_integration: bool,
) -> CommandBuilder {
    #[cfg(windows)]
    if should_inject_explicit_powershell(prog, rest, shell_integration) {
        return powershell_integration_command(std::path::Path::new(prog));
    }

    #[cfg(not(windows))]
    let _ = shell_integration;
    let mut command = CommandBuilder::new(prog);
    if login_shell && prog_accepts_login_flag(prog) {
        command.arg("-l");
    }
    for arg in rest {
        command.arg(arg);
    }
    command
}

/// The kettle PowerShell shell-integration body, embedded so the spawned
/// pwsh can be launched already wired (no `$PROFILE` edit needed).
#[cfg(any(windows, test))]
const POWERSHELL_INTEGRATION: &str = include_str!("../../../shell-integration/kettle.ps1");

/// Is `path` a PowerShell (pwsh / powershell) executable, by basename?
#[cfg(windows)]
fn is_powershell(path: &std::path::Path) -> bool {
    path.to_str().is_some_and(is_powershell_program)
}

/// Windows limits a process command line to 32,767 UTF-16 code units.
/// `-EncodedCommand` base64-encodes UTF-16LE, which would put the integration
/// over that limit. Encoding the source as UTF-8 and decoding it in a fixed
/// ASCII bootstrap keeps the same quoting safety while using roughly half the
/// command line. The compile-time cap leaves room for the executable path,
/// quoting, and future arguments.
#[cfg(windows)]
fn powershell_integration_command(path: &std::path::Path) -> CommandBuilder {
    let mut c = CommandBuilder::new(path);
    c.arg("-NoExit");
    c.arg("-Command");
    c.arg(powershell_integration_bootstrap(POWERSHELL_INTEGRATION));
    c
}

#[cfg(any(windows, test))]
const POWERSHELL_BOOTSTRAP_PREFIX: &str =
    "& ([scriptblock]::Create([Text.Encoding]::UTF8.GetString([Convert]::FromBase64String('";
#[cfg(any(windows, test))]
const POWERSHELL_BOOTSTRAP_SUFFIX: &str = "'))))";
#[cfg(any(windows, test))]
const POWERSHELL_BOOTSTRAP_MAX_CHARS: usize = 32_767 - 3_000;
#[cfg(any(windows, test))]
const POWERSHELL_BOOTSTRAP_GROWTH_HEADROOM: usize = 512;

// Keep the embedded command comfortably below CreateProcessW's 32,767-code-unit
// ceiling. Three thousand code units remain for the executable path, argument
// quoting, and fixed arguments, while the compile-time assertion keeps another
// 512 for integration growth. Base64 expands the exact, unmodified UTF-8 source:
// deleting comment-shaped lines is not semantics-preserving in PowerShell
// because ordinary quoted strings may cross line boundaries. The payload is
// ASCII, so bytes and UTF-16 code units are identical.
#[cfg(any(windows, test))]
const _: () = assert!(
    POWERSHELL_BOOTSTRAP_PREFIX.len()
        + POWERSHELL_INTEGRATION.len().div_ceil(3) * 4
        + POWERSHELL_BOOTSTRAP_SUFFIX.len()
        + POWERSHELL_BOOTSTRAP_GROWTH_HEADROOM
        <= POWERSHELL_BOOTSTRAP_MAX_CHARS
);

#[cfg(any(windows, test))]
fn powershell_integration_bootstrap(script: &str) -> String {
    let encoded = base64_standard(script.as_bytes());
    format!("{POWERSHELL_BOOTSTRAP_PREFIX}{encoded}{POWERSHELL_BOOTSTRAP_SUFFIX}")
}

/// Minimal standard-alphabet base64 encoder (padded). Self-contained so
/// kettle-core takes no base64 dependency for this one-shot spawn-time encode.
#[cfg(any(windows, test))]
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

#[cfg(test)]
mod powershell_bootstrap_tests;

/// Cap on the OSC 133 prompt-mark ring. A long-lived shell session emits one
/// mark per prompt, so without a cap the ring would grow without bound.
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
/// single prompt) and trims oldest-first with O(1) `pop_front`, since this runs
/// on the hot reader-thread path. Pure, so the ring discipline is unit-tested.
fn push_prompt_mark(ring: &mut std::collections::VecDeque<u64>, row_id: u64) -> bool {
    if ring.back() == Some(&row_id) {
        return false;
    }
    ring.push_back(row_id);
    while ring.len() > MAX_PROMPT_MARKS {
        ring.pop_front();
    }
    true
}

/// Re-anchor the active prompt after a column reflow when its start and cursor
/// occupied the same physical row beforehand. Widening cannot split that row.
/// Narrowing is safe only when the stable cursor row did not move, proving the
/// prompt and everything before it gained no wrapped rows. Every ambiguous
/// change clears the ring rather than guessing across wraps.
fn rebase_prompt_marks_after_column_reflow(
    ring: &mut std::collections::VecDeque<u64>,
    old_columns: usize,
    new_columns: usize,
    old_cursor: u64,
    new_cursor: u64,
) {
    let cursor_row_is_safe = new_columns > old_columns || new_cursor == old_cursor;
    let preserve_active = cursor_row_is_safe && ring.back() == Some(&old_cursor);
    ring.clear();
    if preserve_active {
        push_prompt_mark(ring, new_cursor);
    }
}

/// Prune marks that have left the retained grid, then find the display offset
/// of the adjacent prompt (see [`prompt_navigation_target`]).
fn prompt_navigation_offset(
    ring: &mut std::collections::VecDeque<u64>,
    history_origin: u64,
    history_size: usize,
    screen_lines: usize,
    display_offset: usize,
    previous: bool,
) -> Option<usize> {
    let retained_end = history_origin
        .saturating_add(history_size as u64)
        .saturating_add(screen_lines as u64);
    ring.retain(|mark| *mark >= history_origin && *mark < retained_end);
    prompt_navigation_target(
        ring,
        history_origin,
        history_size,
        screen_lines,
        display_offset,
        previous,
    )
}

/// The display offset a jump to the adjacent prompt would scroll to, or `None`
/// when there is no prompt that way. Marks outside the retained grid are
/// skipped rather than pruned, so a caller that only asks (the keyboard
/// ownership check) needs neither a mutable ring nor a copy of it.
fn prompt_navigation_target(
    ring: &std::collections::VecDeque<u64>,
    history_origin: u64,
    history_size: usize,
    screen_lines: usize,
    display_offset: usize,
    previous: bool,
) -> Option<usize> {
    let screen_top = history_origin.saturating_add(history_size as u64);
    let retained_end = screen_top.saturating_add(screen_lines as u64);
    let retained = ring
        .iter()
        .copied()
        .filter(|mark| *mark >= history_origin && *mark < retained_end);

    let current_top = screen_top.saturating_sub(display_offset.min(history_size) as u64);
    let target = if previous {
        retained.filter(|mark| *mark < current_top).max()
    } else {
        retained.filter(|mark| *mark > current_top).min()
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
mod kitty_delete_tests;

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

/// Estimated retained bytes for one scrollback line.
///
/// This counts the inline grid representation only. A cell can also own heap
/// storage (combining marks, an underline color, a hyperlink), which this
/// does not walk: that would touch every cell of every line on each budget
/// evaluation, on the PTY reader's path.
///
/// That is sound only because the dynamic part is separately bounded.
/// Combining marks are capped per cell (`MAX_ZEROWIDTH_PER_CELL`), and
/// hyperlink storage is shared behind an `Arc` across the cells of one link.
/// So the estimate understates by a bounded factor, as `docs/CONFIG.md` tells
/// users.
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
            &ShellStartup::default(),
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
        shell_startup: &ShellStartup,
        capabilities: TerminalCapabilities,
        event_tx: crossbeam_channel::Sender<TermEvent>,
        waker: Waker,
        output_tx: Option<PtyOutputSender>,
    ) -> Result<Terminal> {
        Self::new_with_env_and_output_geometry_capabilities_and_cwd_policy(
            argv,
            cwd.map(std::path::Path::new),
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
            shell_startup,
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
        cwd: Option<&std::path::Path>,
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
        shell_startup: &ShellStartup,
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
        #[cfg(unix)]
        let recording_start_ns = crate::startup_geometry::Trace::begin();
        let pair = pty.openpty(native_pty_size(geometry))?;
        // Observe before command setup or child spawn can correct the PTY.
        #[cfg(unix)]
        let startup_geometry =
            crate::startup_geometry::Trace::start(pair.master.as_raw_fd(), recording_start_ns);

        let mut cmd = pane_command(argv, login_shell, shell_integration);
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
        // Env vars set on the child's *Windows* process do NOT cross into a
        // WSL distro unless listed in `WSLENV`. Without this, `COLORTERM` is
        // silently dropped at the WSL boundary, so a program inside WSL that
        // detects truecolor from `$COLORTERM` falls back to 256-color with
        // washed-out, mis-mapped colors. Append the pane and terminal-identity
        // env vars set above to WSLENV with the `/u` flag ("pass Windows→WSL
        // only"), preserving any the user already set. When the child isn't
        // `wsl.exe`, WSLENV is just an extra, ignored env var.
        cmd.env(
            "WSLENV",
            child_wslenv(&std::env::var("WSLENV").unwrap_or_default(), extra_env),
        );
        // After the environment, which decides the shell the PTY runs and
        // holds the `ZDOTDIR` zsh would have read.
        let explicit = match argv {
            [program] => Some(program.as_str()),
            _ => None,
        };
        apply_working_directory(&mut cmd, cwd, cwd_policy);
        crate::shell_startup::apply(&mut cmd, explicit, shell_startup);
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
        let proxy = EventProxy::with_output_wake_and_osc52(
            event_tx,
            waker.clone(),
            output_wake.clone(),
            osc52_copy_allowed.clone(),
        );
        let lifecycle_pending = proxy.lifecycle_pending();
        let latched = proxy.latched();
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
            xtversion: Some(xtversion()),
            color_scheme_dark: capabilities.color_scheme_dark,
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
        let term = Term::new(tconf.clone(), &TermSize::new(cols, rows), proxy.clone());
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
        let command_generation = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let output_start_seen: Arc<std::sync::atomic::AtomicBool> =
            Arc::new(std::sync::atomic::AtomicBool::new(false));
        let output_start_seen_for_struct = output_start_seen.clone();
        let command_finished: Arc<Mutex<Vec<CommandFinished>>> = Arc::new(Mutex::new(Vec::new()));
        let protocol_notifications: Arc<Mutex<Vec<ProtocolNotification>>> =
            Arc::new(Mutex::new(Vec::new()));
        let completion_cell = Arc::new(Mutex::new(CompletionSlot {
            startup_sync_pending: true,
            ..CompletionSlot::default()
        }));
        // A launch directory that is not UTF-8 cannot seed the text cell; the
        // OS-derived fallback below supplies the directory instead.
        let cwd_cell: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(
            cwd.and_then(std::path::Path::to_str).map(str::to_owned),
        ));
        // OS-derived cwd fallback, populated by the App's process poll for
        // native shells with no OSC 7/9;9.
        let native_cwd_cell: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        // Set once the shell actually REPORTS a cwd via OSC 7/9;9, which makes the
        // OSC cwd authoritative. `cwd_cell` is pre-seeded with the launch directory
        // (above), so without this flag `current_dir_or_native` would always prefer
        // that frozen seed over the live native poll, leaving the tab stuck at the
        // launch dir for a stock shell that emits no OSC 7.
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
        // Per-pane output-generation counter (see the `out_gen` struct field).
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

        let (local_output, local_output_rx) =
            crossbeam_channel::bounded::<LocalOutput>(LOCAL_OUTPUT_QUEUE_DEPTH);

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
            let command_generation = command_generation.clone();
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
                    // Built before the child spawns, so it sees the hostname
                    // the shell is about to read.
                    let mut extractor = Extractor::for_shell();
                    let mut private_output_filter = PrivateOutputFilter::default();
                    let mut active_alternate = false;
                    let mut observed_reflow_generation = 0;
                    let mut session_log_filter = SessionLogFilter::default();
                    // A blocking PTY read must remain on a pump thread so the
                    // parser's DEC 2026 timeout can wake independently. Bound
                    // the handoff and recycle buffers: under output flood this
                    // applies backpressure instead of retaining an unbounded
                    // queue of fresh 64 KiB allocations.
                    let (raw_tx, raw_rx) = crossbeam_channel::bounded::<
                        Option<(u64, Vec<u8>, usize)>,
                    >(PTY_PUMP_QUEUE_DEPTH);
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
                                            // Publish activity before this chunk can block behind
                                            // either the parser queue or a lossless raw-output
                                            // subscriber. ConPTY completion must not treat that
                                            // hidden work as a quiet transport.
                                            let read_generation = pump_progress.mark_chunk_read();
                                            match forward_pty_buffer_or_drain(
                                                &raw_tx,
                                                &pump_drain_output,
                                                read_generation,
                                                buffer,
                                                n,
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
                    let mut local_output_rx = local_output_rx;
                    let mut pending_local = None;
                    let mut handled_generation = 0;
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
                                        inactive: &inactive_graphics,
                                        images: &images,
                                        virtuals: &virtuals,
                                        anims: &anims,
                                        relatives: &relatives,
                                        geometry: &shared_geometry,
                                        proxy: &proxy,
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
                            receive_parser_input(
                                &mut processor,
                                &raw_rx,
                                &mut local_output_rx,
                                &mut pending_local,
                                handled_generation,
                                &mut sync_flush,
                            )
                        };
                        match received {
                            None => {
                                let tap_raw = log_active.load(Ordering::Relaxed)
                                    || output_tx.is_some();
                                let final_raw = private_output_filter.finish(tap_raw);
                                if !final_raw.is_empty() {
                                    if log_active.load(Ordering::Relaxed)
                                        && let Ok(mut guard) = session_log.lock()
                                        && let Some(writer) = guard.as_mut()
                                    {
                                        let strip = log_strip_ansi
                                            .lock()
                                            .map(|value| *value)
                                            .unwrap_or(false);
                                        let generation = log_generation.load(Ordering::Acquire);
                                        let filtered = session_log_filter.filter(
                                            &final_raw,
                                            generation,
                                            strip,
                                        );
                                        if writer.try_write(filtered).is_err() {
                                            log_generation.fetch_add(1, Ordering::AcqRel);
                                            log_active.store(false, Ordering::Release);
                                        }
                                    }
                                    if let Some(tx) = &output_tx {
                                        tx.send(final_raw);
                                    }
                                }
                                proxy.send_event_exit();
                                break;
                            }
                            Some(input) => {
                                if stop.load(Ordering::Relaxed) {
                                    break;
                                }
                                let (read_generation, buffer, n, from_child) = match input {
                                    ParserInput::Pty(generation, buffer, n) => {
                                        handled_generation = generation;
                                        (generation, buffer, n, true)
                                    }
                                    ParserInput::Local(bytes) => {
                                        let n = bytes.len();
                                        (reader_progress.load().generation, bytes, n, false)
                                    }
                                };
                                // Evaluated once per PTY read. If logging starts
                                // while a bounded control string is in flight,
                                // the extractor publishes the complete sequence
                                // when its terminator arrives instead of a
                                // malformed suffix. Local output never reaches
                                // the session log or the recorder, which hold
                                // what the child wrote.
                                let tap_raw = log_active
                                    .load(std::sync::atomic::Ordering::Relaxed)
                                    || output_tx.is_some();
                                let raw_output = if from_child {
                                    private_output_filter.feed(&buffer[..n], tap_raw)
                                } else {
                                    Vec::new()
                                };
                                if !raw_output.is_empty() {
                                    // Publish at most once per PTY read. OSC-heavy output can
                                    // produce thousands of parser chunks, while the logger and
                                    // recorder queues are intentionally bounded in read-sized
                                    // messages. Kettle's private completion OSC is the only data
                                    // removed by `PrivateOutputFilter`.
                                    if log_active.load(Ordering::Relaxed)
                                        && let Ok(mut guard) = session_log.lock()
                                        && let Some(writer) = guard.as_mut()
                                    {
                                        let strip = log_strip_ansi
                                            .lock()
                                            .map(|value| *value)
                                            .unwrap_or(false);
                                        let generation = log_generation.load(Ordering::Acquire);
                                        let filtered = session_log_filter.filter(
                                            &raw_output,
                                            generation,
                                            strip,
                                        );
                                        if writer.try_write(filtered).is_err() {
                                            log_generation.fetch_add(1, Ordering::AcqRel);
                                            log_active.store(false, Ordering::Release);
                                        }
                                    }
                                    if let Some(tx) = &output_tx {
                                        tx.send(raw_output);
                                    }
                                }
                                // Raw consumers are handled once above; parser chunks now serve
                                // only the terminal and semantic side channels.
                                // Local output skips the extractor; see below.
                                let child_bytes: &[u8] =
                                    if from_child { &buffer[..n] } else { &[] };
                                extractor.set_raw_tap(false);
                                extractor.feed_with(child_bytes, |extractor, chunk| {
                                    let chunk = match chunk {
                                        Chunk::Raw(_) => return,
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
                                        Chunk::GraphicsReply(reply) => send_graphics_reply(&proxy, reply),
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
                                                    inactive: &inactive_graphics,
                                                    images: &images,
                                                    virtuals: &virtuals,
                                                    anims: &anims,
                                                    relatives: &relatives,
                                                    geometry: &shared_geometry,
                                                    proxy: &proxy,
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
                                        Chunk::ImageQuota(request) => {
                                            apply_image_quota(
                                                request,
                                                GraphicsActionContext {
                                                    inactive: &inactive_graphics,
                                                    images: &images,
                                                    virtuals: &virtuals,
                                                    anims: &anims,
                                                    relatives: &relatives,
                                                    geometry: &shared_geometry,
                                                    proxy: &proxy,
                                                },
                                                extractor,
                                            );
                                        }
                                        Chunk::Image(mut placed) => {
                                            let key = placed.id.map(|id| (id, placed.placement_id));
                                            let completion = placed.completion.take();
                                            let result = place_image(&term, &images, &shared_geometry, &mut processor, placed);
                                            let admission = result.as_ref().map(|_| ()).map_err(|error| *error);
                                            if admission.is_ok() {
                                                reconcile_named_placement(&images, &virtuals, &relatives, key, PlacementForm::Physical);
                                            }
                                            if let Ok(Some(batch)) = result {
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
                                            finish_graphics_placement(extractor, &proxy, completion, admission);
                                        }
                                        // This arm's depth makes rustfmt reflow unrelated PTY
                                        // reader code whenever its body changes.
                                        #[rustfmt::skip]
                                        Chunk::DeleteImages(delete) => {
                                            let relative_snapshot = relatives
                                                .lock()
                                                .map(|placements| placements.clone())
                                                .unwrap_or_default();
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
                                                                if relative_snapshot.is_empty() {
                                                                    Vec::new()
                                                                } else {
                                                                    Terminal::placeholder_cells_from_term(
                                                                        &t,
                                                                    )
                                                                },
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
                                            let resolver = relative_resolver(
                                                &images,
                                                &virtuals,
                                                &placeholder_cells,
                                                relative_snapshot.iter().map(|(&key, entry)| (key, entry)),
                                            );
                                            let relative_positions = relative_snapshot
                                                .iter()
                                                .filter_map(|(&(id, placement_id), entry)| {
                                                    let (parent_abs, parent_col) = resolver.resolve(
                                                        PlacementKey {
                                                            image_id: entry.parent_img,
                                                            placement_id: entry.parent_placement,
                                                        },
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
                                                    |&(image_id, placement_id), entry| {
                                                        let matched = matches!(delete.target, KittyDeleteTarget::ZIndex { z } if entry.z == z)
                                                            || relative_positions
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

                                                cascade_removed_relatives(
                                                    &mut relative_placements,
                                                    &mut removed_keys,
                                                    &mut removed_ids,
                                                );
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
                                        chunk @ (Chunk::RelativePlacement { .. } | Chunk::VirtualImage { .. }) => {
                                            let mut current = term.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                                            apply_graphics_chunk_at(
                                                &mut current,
                                                chunk,
                                                GraphicsActionContext {
                                                    inactive: &inactive_graphics,
                                                    images: &images,
                                                    virtuals: &virtuals,
                                                    anims: &anims,
                                                    relatives: &relatives,
                                                    geometry: &shared_geometry,
                                                    proxy: &proxy,
                                                },
                                                extractor,
                                            );
                                        }
                                        Chunk::Animation {
                                            id,
                                            imgs,
                                            gaps,
                                            state,
                                        } => {
                                            apply_animation_snapshot(
                                                id,
                                                imgs,
                                                gaps,
                                                state,
                                                GraphicsActionContext {
                                                    inactive: &inactive_graphics,
                                                    images: &images,
                                                    virtuals: &virtuals,
                                                    anims: &anims,
                                                    relatives: &relatives,
                                                    geometry: &shared_geometry,
                                                    proxy: &proxy,
                                                },
                                            );
                                        }
                                        Chunk::Prompt(PromptKind::PromptStart) => {
                                            let prompt_row = if let Ok(t) = term.lock()
                                                && !t.mode().contains(
                                                    alacritty_terminal::term::TermMode::ALT_SCREEN,
                                                ) {
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
                                                    Some((row_id, grid.history_origin()))
                                            } else {
                                                None
                                            };
                                            // Fish 4 can emit the same prompt-start mark twice:
                                            // once from the bundled event handler and once while
                                            // it renders the prompt. Resetting between the managed
                                            // `sync` and its first completion reply rejects the
                                            // reply after Fish has already suppressed its pager.
                                            // The prompt ring already identifies that duplicate by
                                            // stable row id, so use the same decision for the
                                            // completion session. Locks above are gone before the
                                            // completion lock is taken.
                                            apply_completion_reader_prompt_start(
                                                &completion_cell,
                                                &prompts,
                                                prompt_row,
                                                read_generation,
                                            );
                                        }
                                        // Terminator parity (command_notify.py):
                                        // OSC 133 OutputStart (C) marks the moment the
                                        // shell handed control to a user command. Record
                                        // the timestamp so the matching CommandEnd (D)
                                        // can compute the elapsed duration.
                                        Chunk::Prompt(PromptKind::OutputStart) => {
                                            if let Ok(mut completion) = completion_cell.lock() {
                                                clear_completion_slot(&mut completion);
                                                completion.prompt_boundary_pending = true;
                                            }
                                            // One command line is one command: bash's
                                            // DEBUG trap marks C before every simple
                                            // command and again before PROMPT_COMMAND, so
                                            // only the first C since the last D or prompt
                                            // starts its generation and its duration.
                                            if let Ok(mut t) = output_started_at.lock()
                                                && t.is_none()
                                            {
                                                command_generation
                                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                                *t = Some(std::time::Instant::now());
                                            }
                                            // `shell_idle` needs a REAL OutputStart source.
                                            // Prompt marks alone never authorize a
                                            // close-confirm skip, or an A/B/D-only
                                            // integration would look permanently idle.
                                            output_start_seen
                                                .store(true, std::sync::atomic::Ordering::Relaxed);
                                        }
                                        // OSC 133 CommandEnd (D). Take the
                                        // command's OutputStart timestamp, compute
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
                                                    generation: command_generation
                                                        .load(std::sync::atomic::Ordering::Relaxed),
                                                    duration: started.elapsed(),
                                                    exit_code: code,
                                                });
                                            }
                                        }
                                        // OSC 133 B marks "end of prompt / input start".
                                        // PS1/prompt emits it AFTER every PROMPT_COMMAND
                                        // segment runs, so it definitively means the shell
                                        // is back at a prompt. Clearing here un-sticks
                                        // `output_started_at` when a user's pre-existing
                                        // PROMPT_COMMAND echoes through the bash DEBUG trap
                                        // (which fires a stray C after our D). Otherwise
                                        // the pane stays "running" and the prompt-aware
                                        // close-confirm skip never applies. B is not a
                                        // command end, so no CommandFinished is pushed.
                                        Chunk::Prompt(PromptKind::CommandStart) => {
                                            if let Ok(mut completion) = completion_cell.lock() {
                                                hide_completion_view_slot(&mut completion);
                                            }
                                            if let Ok(mut t) = output_started_at.lock() {
                                                *t = None;
                                            }
                                        }
                                        Chunk::Cwd(path) => {
                                            if let Ok(mut c) = cwd_cell.lock() {
                                                *c = Some(path);
                                            }
                                            // The shell reported a real cwd, so the OSC
                                            // cwd now outranks the native poll.
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
                                // The extractor may hold an unfinished control
                                // string from the child, such as a private
                                // completion OSC, which would swallow local
                                // bytes. The VT parser never sees a string the
                                // extractor holds, so parsing local output there
                                // applies it at once and leaves the child's
                                // string captured.
                                if !from_child {
                                    let _graphics_guard = graphics_gate
                                        .lock()
                                        .unwrap_or_else(|poisoned| poisoned.into_inner());
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
                                            inactive: &inactive_graphics,
                                            images: &images,
                                            virtuals: &virtuals,
                                            anims: &anims,
                                            relatives: &relatives,
                                            geometry: &shared_geometry,
                                            proxy: &proxy,
                                        },
                                        extractor: &mut extractor,
                                    };
                                    advance_terminal_bytes(
                                        &mut processor,
                                        &term,
                                        &buffer[..n],
                                        &mut sync_graphics,
                                    );
                                }
                                image_pruner.prune_if_changed(&term, &images);
                                // Local buffers are not the pump's 64 KiB read
                                // buffers and were never counted as reads.
                                if from_child {
                                    let _ = recycle_tx.try_send(buffer);
                                }
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
                                if from_child {
                                    reader_progress.mark_chunk_handled();
                                }
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
            #[cfg(unix)]
            startup_geometry,
            #[cfg(windows)]
            pty_close: None,
            writer: Arc::new(Mutex::new(writer)),
            local_output,
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
            command_generation,
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
            latched,
            osc52_copy_allowed,
            out_gen,
        })
    }

    /// Monotone counter of PTY reads this pane's reader thread has processed.
    /// A UI that recorded the value at its last paint can answer "any output
    /// since?" without draining the event channel (plain text emits no
    /// `TermEvent`). Bumped with `Release` before the wakeup fires; read with
    /// `Acquire`.
    pub fn output_generation(&self) -> u64 {
        self.out_gen.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Monotone counter of this pane's geometry commits: every resize,
    /// including the reflow a column change causes. Output never changes it,
    /// so a cache of positions in the visible grid (detected links, for one)
    /// keys on it alongside [`Self::output_generation`]: a resize moves text
    /// without any output.
    pub fn geometry_generation(&self) -> u64 {
        self.geometry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .generation
    }

    /// Parse `bytes` as terminal output without sending them to the child.
    ///
    /// Reset (RIS) and Clear Scrollback (CSI 3 J) change the terminal, not
    /// the program in it. The reader parses these bytes once it has handled
    /// every PTY read that finished before this call; a read finishing at the
    /// same moment may land on either side, like output arriving just after
    /// it. Graphics, synchronized output and redraws behave as if the child had
    /// written them. They skip the extractor, so a control string the child
    /// left unfinished cannot absorb them. They are not added to the session
    /// log or the recording.
    /// Returns `false` when the reader has stopped or already holds
    /// `LOCAL_OUTPUT_QUEUE_DEPTH` requests.
    pub fn apply_local_output(&self, bytes: &[u8]) -> bool {
        let generation = self.pty_read_progress.load().generation;
        self.local_output
            .try_send((generation, bytes.to_vec()))
            .is_ok()
    }

    /// Update whether DA1 may advertise OSC 52 clipboard writes.
    ///
    /// The event proxy reads this atomically while parsing a DA request, so a
    /// live config reload takes effect without restarting the pane.
    pub fn set_osc52_copy_allowed(&self, allowed: bool) {
        self.osc52_copy_allowed.store(allowed, Ordering::Release);
    }

    /// The colours changed: record whether the scheme is dark, and report it
    /// at once (`CSI ? 997 ; 1|2 n`) to a program that turned on DEC mode 2031.
    /// Any program can ask with `CSI ? 996 n`.
    pub fn set_color_scheme(&self, dark: Option<bool>) {
        if let Ok(mut term) = self.term.lock() {
            term.set_color_scheme(dark);
        }
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

    /// The path of this pane's terminal device, the pseudo-terminal's other
    /// end, named when it was opened. It names that device while Kettle holds
    /// the controller, which it does for the pane's life.
    #[cfg(unix)]
    pub fn tty_path(&self) -> Option<std::path::PathBuf> {
        self.master.as_ref()?.tty_name()
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

    /// The last command generation, only when no OSC 133 command is running.
    pub fn idle_command_generation(&self) -> Option<u64> {
        command_admission_generation(&self.output_started_at, &self.command_generation)
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

    /// Whether this pane's shell is sitting IDLE at a prompt (Ghostty
    /// `confirm-close-surface` parity). True only when shell integration has
    /// been observed (≥1 OSC 133 prompt mark and an OutputStart) AND no command
    /// is currently running. `output_started_at` is the OutputStart→CommandEnd
    /// window, so a full-screen app like vim counts as running until it exits.
    /// Without integration this is always `false`, so close-confirmation
    /// behavior is byte-identical for plain shells; a command whose
    /// CommandEnd never arrives stays "running", which errs toward asking.
    pub fn shell_idle(&self) -> bool {
        self.shell_activity() == ShellActivity::Idle
    }

    /// Whether shell integration shows the shell at its prompt: at least one
    /// prompt mark has arrived and no command has started since the last
    /// one. Unlike [`shell_idle`](Self::shell_idle), this does not wait for a
    /// first OutputStart, so a fresh prompt already counts; B clears a stray
    /// OutputStart the way it does for `shell_idle`. Without integration it
    /// is always false.
    pub fn shell_at_prompt(&self) -> bool {
        let seen_prompts = self.prompts.lock().map(|p| !p.is_empty()).unwrap_or(false);
        seen_prompts
            && self
                .output_started_at
                .lock()
                .map(|started| started.is_none())
                .unwrap_or(false)
    }

    /// What this pane's terminal says about who should get a key that both
    /// Kettle and a program could use (see [`KeyboardClaims`]). Read only for
    /// chords that can yield, so it costs nothing on ordinary typing. Locks the
    /// term, then the prompt ring, in the reader thread's order, as
    /// `jump_to_prompt` does, and only reads the ring: nothing is pruned or
    /// moved. A poisoned term lock reports nothing, which keeps every chord
    /// Kettle's.
    pub fn keyboard_claims(&self) -> KeyboardClaims {
        use alacritty_terminal::term::TermMode;

        let at_prompt = self.shell_at_prompt();
        let Ok(term) = self.term.lock() else {
            return KeyboardClaims {
                at_prompt,
                ..KeyboardClaims::default()
            };
        };
        let mode = *term.mode();
        let alt_screen = mode.contains(TermMode::ALT_SCREEN);
        let grid = term.grid();
        let (history_origin, history_size, screen_lines, display_offset) = (
            grid.history_origin(),
            grid.history_size(),
            grid.screen_lines(),
            grid.display_offset(),
        );
        let (prompt_above, prompt_below) = if alt_screen {
            // Prompt ids belong to the primary grid (see `prompt_marks`).
            (false, false)
        } else {
            self.prompts
                .lock()
                .map(|marks| {
                    // A jump counts only if it would move the view: a prompt
                    // already on screen at the bottom maps back to offset 0.
                    let can_jump = |previous| {
                        prompt_navigation_target(
                            &marks,
                            history_origin,
                            history_size,
                            screen_lines,
                            display_offset,
                            previous,
                        )
                        .is_some_and(|offset| offset != display_offset)
                    };
                    (can_jump(true), can_jump(false))
                })
                .unwrap_or((false, false))
        };
        KeyboardClaims {
            keyboard_protocol: mode
                .intersects(TermMode::KITTY_KEYBOARD_PROTOCOL | TermMode::MODIFY_OTHER_KEYS),
            mouse_mode: mode.intersects(TermMode::MOUSE_MODE),
            alt_screen,
            history_size,
            display_offset,
            prompt_above,
            prompt_below,
            at_prompt,
        }
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

    /// The last OSC 7/9;9 report, or the launch directory before one. Private,
    /// because it misses every `cd` in a shell that never reports. Use
    /// [`current_dir_or_native`](Self::current_dir_or_native) instead.
    fn reported_or_launch_dir(&self) -> Option<String> {
        self.cwd.lock().ok().and_then(|c| c.clone())
    }

    /// Working directory explicitly reported by the child via OSC 7/9;9.
    ///
    /// Never the launch-directory seed and never an OS read. Use this when a
    /// caller must not trust an OS read, such as WSL split-cloning.
    pub fn reported_current_dir(&self) -> Option<String> {
        reported_current_dir(
            self.osc_cwd_seen.load(std::sync::atomic::Ordering::Relaxed),
            self.reported_or_launch_dir(),
        )
    }

    /// Set the OS-derived native cwd fallback, which the App's process poll
    /// writes for native shells lacking OSC 7/9;9. `None` clears it.
    pub fn set_native_cwd(&self, dir: Option<String>) {
        if let Ok(mut c) = self.native_cwd.lock() {
            *c = dir;
        }
    }

    /// Where this pane's shell is now. Labels, new panes and tabs, session save
    /// and ctl all read this, so a split opens where the label says.
    ///
    /// If the shell has actually REPORTED a cwd via OSC 7/9;9 (`osc_cwd_seen`),
    /// that is authoritative — return it, so a shell that volunteers its directory
    /// (including WSL, where the native Windows read is meaningless) is unaffected.
    ///
    /// Otherwise the only value in `cwd` is the pre-seeded *launch* directory,
    /// which never tracks `cd`; prefer the live OS-derived `native_cwd` poll
    /// (which does), falling back to that launch seed until the first poll lands.
    /// Preferring `cwd` here would let the seeded launch dir shadow the native
    /// poll and freeze a stock Windows shell's tab.
    pub fn current_dir_or_native(&self) -> Option<String> {
        if self.osc_cwd_seen.load(std::sync::atomic::Ordering::Relaxed) {
            return self.reported_or_launch_dir();
        }
        self.native_cwd
            .lock()
            .ok()
            .and_then(|c| c.clone())
            .or_else(|| self.reported_or_launch_dir())
    }

    /// Latest OSC 9;4 taskbar-progress state reported by this pane
    /// (`None` if never reported, or explicitly cleared with state 0). The
    /// App polls the focused pane's value each frame to drive the OS taskbar.
    pub fn progress(&self) -> Option<Progress> {
        self.progress.lock().ok().and_then(|g| *g)
    }

    /// PTY child PID accessor. Returns the immutable OS pid captured when the
    /// pane was spawned. `None` means the platform does not expose a pid for
    /// this Child type (the Windows fallback path).
    ///
    /// The remote-session detector roots its process-tree walk at this captured child PID.
    /// Read-only; does not consume the Child.
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
        self.completion_projection().map(|(list, _)| list)
    }

    /// Visible completion plus the cursor column captured before its first
    /// candidate changed the editor buffer. The UI combines that stable point
    /// with protocol v4's bounded input prefix to align the card with the
    /// editable command column rather than the pane edge.
    pub fn completion_projection(&self) -> Option<(CompletionList, Option<usize>)> {
        let now = std::time::Instant::now();
        self.completion.lock().ok().and_then(|slot| {
            completion_visible(&slot, now)
                .cloned()
                .map(|list| (list, slot.anchor_cursor_col))
        })
    }

    /// Dismiss a visible completion card after a pointer press on the card.
    pub fn clear_completion(&self) {
        if let Ok(mut completion) = self.completion.lock() {
            dismiss_completion_slot(&mut completion);
        }
    }

    /// Clear completion UI across an application-focus boundary. Legacy
    /// unnumbered publishers remain blocked until the next prompt; managed
    /// request-numbered adapters can safely resume on the next Tab.
    pub fn invalidate_completion(&self) {
        if let Ok(mut completion) = self.completion.lock() {
            let read_generation = self.pty_read_progress.load().generation;
            invalidate_completion_slot(&mut completion, read_generation);
        }
    }

    /// Apply completion state changes atomically with admission to the PTY
    /// input queue. The completion lock stays held across the non-blocking
    /// enqueue and the state commit, so a fast shell reply cannot apply before
    /// its request is armed. A rejected enqueue changes nothing, so
    /// backpressure cannot consume a request number the shell never received.
    ///
    /// `inputs` preserves key boundaries for batched control input. A remote
    /// `send_keys` request can contain several Tab presses in one queue item;
    /// the shell observes each key and increments once per key, so Kettle must
    /// do the same rather than classifying only the concatenated byte string.
    pub fn with_completion_input_admission<R>(
        &self,
        inputs: &[&[u8]],
        admit: impl FnOnce() -> (R, bool),
    ) -> R {
        let cursor_col = self
            .term
            .lock()
            .ok()
            .map(|term| term.grid().cursor.point.column.0);
        let Ok(mut completion) = self.completion.lock() else {
            return admit().0;
        };
        with_completion_input_admission_slot_at(
            &mut completion,
            inputs,
            std::time::Instant::now(),
            cursor_col,
            admit,
        )
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

    /// Visible row span occupied by the active prompt and command line.
    ///
    /// Completion UI uses the OSC 133 prompt start rather than guessing from
    /// the cursor alone: a two-line prompt or wrapped input occupies every row
    /// between these endpoints and must not be painted over. `None` means the
    /// current prompt start is not retained and no overlap-safe projection can
    /// be proven.
    pub fn completion_command_rows(&self) -> Option<(usize, usize)> {
        let term = self.term.lock().ok()?;
        if term
            .mode()
            .contains(alacritty_terminal::term::TermMode::ALT_SCREEN)
        {
            return None;
        }
        let grid = term.grid();
        let visible_top = grid
            .history_origin()
            .saturating_add(grid.history_size() as u64)
            .saturating_sub(grid.display_offset() as u64);
        let cursor = stable_grid_line_id(
            grid.history_origin(),
            grid.history_size(),
            grid.cursor.point.line.0,
        );
        let screen_lines = grid.screen_lines();
        let prompts = self.prompts.lock().ok()?;
        let prompt = prompts
            .iter()
            .rev()
            .copied()
            .find(|mark| *mark >= visible_top && *mark <= cursor)?;
        let prompt_row = usize::try_from(prompt - visible_top).ok()?;
        let cursor_row = usize::try_from(cursor - visible_top).ok()?;
        (cursor_row < screen_lines && prompt_row <= cursor_row).then_some((prompt_row, cursor_row))
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

    fn resolve_animation_frames<'a>(
        &self,
        images: impl IntoIterator<Item = (Option<u32>, &'a mut crate::ImageData)>,
    ) {
        if let Ok(animations) = self.anims.lock()
            && !animations.is_empty()
        {
            for (id, image) in images {
                if let Some(id) = id
                    && let Some(entry) = animations.get(&id)
                    && let Some(frame) = entry.current()
                {
                    *image = frame.clone();
                }
            }
        }
    }

    /// Image placements for this terminal (cloned cheaply; `ImageData` is
    /// `Arc`-backed). For placements whose kitty id has a registered
    /// animation, the image is swapped for the frame the playback clock
    /// selects right now, so animations play wherever the image sits.
    pub fn placements(&self) -> Vec<Placement> {
        let mut v = self.images.lock().map(|v| v.clone()).unwrap_or_default();
        self.apply_animation_frames(&mut v);
        v
    }

    /// Swap each placement's image for the frame its animation's playback
    /// clock selects now, wherever and however it is placed.
    fn apply_animation_frames(&self, placements: &mut [Placement]) {
        self.resolve_animation_frames(placements.iter_mut().map(|p| (p.id, &mut p.img)));
    }

    /// `true` if any registered kitty animation is currently running (so the
    /// UI knows to schedule frame-paced redraws).
    pub fn has_running_animation(&self) -> bool {
        self.anims
            .lock()
            .map(|animations| {
                animations.values().any(|entry| {
                    kettle_vt::kitty::playback_state(
                        &entry.gaps,
                        &entry.state,
                        entry.started.elapsed().as_millis(),
                    )
                    .1
                })
            })
            .unwrap_or(false)
    }

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
        let placement_limit = kettle_vt::GraphicsLimits::default().placements;
        let mut placeholder_count = 0usize;
        for ind in content.display_iter {
            let (cell, p) = (ind.cell, ind.point);
            if placeholder::is_placeholder(cell.c)
                && cell.zerowidth().is_none_or(|marks| marks.len() <= 4)
            {
                if placeholder_count >= placement_limit {
                    break;
                }
                let contiguous = matches!(
                    last,
                    Some((r, c)) if r == p.line.0 && c + 1 == p.column.0 as i32
                );
                if !contiguous || runs.is_empty() {
                    runs.push(Vec::new());
                }
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
                            diacritics: CellDiacritics::parse(cell.zerowidth().unwrap_or(&[])),
                        },
                        stable_grid_line_id(history_origin, history_size, p.line.0),
                        p.column.0,
                    ));
                    placeholder_count += 1;
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
        // deterministic. Resolving that per cell would rescan every virtual,
        // O(cells x virtuals), up to 256x256 scans in a single frame. One pass
        // builds the answer for every image instead.
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
        // grid under the `term` lock, so testing `virtuals` afterwards would pay
        // for a full scan of every pane on every frame to reach an empty map.
        let Some((mut virtuals, smallest_for_image)) = self.virtuals_snapshot() else {
            return Vec::new();
        };
        self.resolve_animation_frames(
            virtuals
                .iter_mut()
                .map(|(&(id, _), entry)| (Some(id), &mut entry.img)),
        );

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
        self.apply_animation_frames(&mut out);
        out
    }

    /// Placements for kitty relative placements, drawn `(h, v)` cells from
    /// their parent's origin. That origin is the top-left of the parent's
    /// visible placeholder cells or its regular placement, and chains of
    /// relative parents resolve up to depth 8. A relative whose parent has no
    /// origin this frame is skipped (simply not shown).
    pub fn relative_tiles(&self) -> Vec<Placement> {
        // Snapshot the relatives, then drop the lock before taking the
        // grid / images locks (keeps a single lock-acquisition order).
        let mut entries: Vec<(u32, u32, RelEntry)> = {
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
        self.resolve_animation_frames(
            entries
                .iter_mut()
                .map(|(id, _, entry)| (Some(*id), &mut entry.img)),
        );
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
        let resolver = relative_resolver(
            &self.images,
            &self.virtuals,
            &placeholder_cells,
            entries
                .iter()
                .map(|(image_id, placement_id, entry)| ((*image_id, *placement_id), entry)),
        );
        let mut out = Vec::new();
        for (cimg, placement_id, e) in &entries {
            // kitty requires a chain depth of at least 8.
            let Some((pa, pc)) = resolver.resolve(
                PlacementKey {
                    image_id: e.parent_img,
                    placement_id: e.parent_placement,
                },
                8,
            ) else {
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
        self.apply_animation_frames(&mut out);
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

    /// A cloneable, `Send + Sync` handle to the PTY's write side. `Terminal`
    /// itself is not `Sync` (it owns the `Send`-only master), so a worker
    /// thread (e.g. `kettle exec`'s stdin pump) can't share the whole engine,
    /// but it CAN hold a `PtyWriter` to feed input. The handle keeps writing
    /// valid bytes even after the `Terminal` is dropped (Drop swaps the writer
    /// for a discard sink).
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
    /// Unlike resize, an edited budget may discard old history immediately.
    /// Resize defers a lower limit until the next history-producing scroll.
    ///
    /// A config reload calls this, whether the edit came from the config file
    /// or the Settings overlay's two scrollback rows, so running panes pick up
    /// the new budget instead of keeping the one read at spawn.
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
            self.record_startup_resize(desired, false, &Ok(()));
            return Ok(());
        }

        // Graphics chunks take this gate before observing Term -> geometry.
        // Taking it before the local grid commit makes reflow invalidation and
        // placement insertion a single ordered operation.
        let _graphics_guard = self
            .graphics_gate
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let grid_options = if grid_changed {
            self.term_config.scrolling_history = effective_scrollback_lines(
                self.scrollback_line_limit,
                self.scrollback_byte_limit,
                desired.columns,
                desired.rows,
            );
            Some(self.term_config.clone())
        } else {
            None
        };
        commit_local_geometry_inner(
            &self.term,
            &self.geometry,
            Some(&self.prompts),
            desired,
            grid_options,
        );
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
        self.cols = desired.columns;
        self.rows = desired.rows;
        drop(_graphics_guard);

        let native_result = if resize_native {
            self.master
                .as_ref()
                .map_or(Ok(()), |master| master.resize(native_pty_size(desired)))
                .context("native pseudoterminal resize")
        } else {
            Ok(())
        };
        self.record_startup_resize(desired, resize_native, &native_result);
        if native_result.is_ok() {
            // On Windows a pixel-only update deliberately skips
            // ResizePseudoConsole; recording the desired pixels here is safe
            // because future native comparisons ignore them on that platform.
            self.applied_pty_geometry = desired;
        }

        native_result
    }

    fn record_startup_resize(&mut self, desired: PtyGeometry, resized: bool, result: &Result<()>) {
        #[cfg(unix)]
        if let Some(trace) = self.startup_geometry.as_mut() {
            use crate::startup_geometry::Outcome;
            let outcome = if result.is_err() {
                Outcome::Error
            } else if resized {
                Outcome::Ok
            } else {
                Outcome::Noop
            };
            // The vendored Unix backend formats ioctl errors into anyhow.
            // -1 means unavailable; never log the free-form error text here.
            let native_error = result.as_ref().err().map(|error| {
                error
                    .downcast_ref::<std::io::Error>()
                    .and_then(std::io::Error::raw_os_error)
                    .unwrap_or(-1)
            });
            trace.record(
                self.master.as_ref().and_then(|m| m.as_raw_fd()),
                native_pty_size(desired),
                outcome,
                native_error,
            );
        }
        #[cfg(not(unix))]
        let _ = (desired, resized, result);
    }

    /// Link the optional startup diagnostic to the owning UI pane.
    pub fn link_startup_geometry(&mut self, pane_id: u64) {
        #[cfg(unix)]
        if let Some(trace) = self.startup_geometry.as_mut() {
            trace.link(pane_id, self.child_pid);
        }
        #[cfg(not(unix))]
        let _ = pane_id;
    }

    /// Finish a diagnostic interval, or return its one remaining wait budget.
    /// Called before reaping, including for hidden windows. No extra thread.
    pub fn poll_startup_geometry(&mut self) -> Option<std::time::Duration> {
        #[cfg(unix)]
        if let Some(trace) = self.startup_geometry.as_ref() {
            let wait = trace.remaining();
            if !wait.is_zero() {
                return Some(wait);
            }
            let alive = self
                .child
                .lock()
                .ok()
                .and_then(|mut child| child.try_wait().ok())
                .is_some_and(|status| status.is_none());
            if let Some(mut trace) = self.startup_geometry.take() {
                trace.finish(self.master.as_ref().and_then(|m| m.as_raw_fd()), alive);
            }
        }
        None
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

    /// Whether the program rang the bell since the last call. Bells are
    /// latched rather than queued, so any number of them is one bell here.
    pub fn take_bell(&self) -> bool {
        self.latched.take_bell()
    }

    /// The newest title the program set or reset since the last call.
    pub fn take_title(&self) -> Option<crate::TitleChange> {
        self.latched.take_title()
    }

    /// Whether the program switched cursor blinking (DEC mode 12) since the
    /// last call.
    pub fn take_cursor_blinking_changed(&self) -> bool {
        self.latched.take_cursor_blinking_changed()
    }

    /// The newest OSC 52 write for each target since the last call.
    pub fn take_clipboard_stores(&self) -> Vec<(crate::ClipboardType, String)> {
        self.latched.take_clipboard_stores()
    }

    /// Whether the reader finished: the PTY closed after the last of its
    /// output crossed the parser. Taken once; the owner applies exit policy
    /// after handling the replies queued before it.
    pub fn take_exited(&self) -> bool {
        self.latched.take_exited()
    }

    /// Has the child process exited?
    pub fn child_exited(&self) -> bool {
        self.child
            .lock()
            .ok()
            .and_then(|mut c| c.try_wait().ok().flatten())
            .is_some()
    }

    /// Kill the child immediately.
    /// Used by `kettle exec --timeout` when the deadline fires. The reader
    /// thread sees EOF when the master closes on drop, so no extra teardown is
    /// needed here.
    ///
    /// A child that had already exited counts as success, since that is what
    /// the caller wanted. A real failure to terminate is returned, because the
    /// process is still running and a caller about to report a timeout should
    /// be able to say so.
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

    /// The child's exit status, if it has exited (non-blocking `try_wait`).
    /// `child_exited` discards the status; the headless `kettle exec` path
    /// needs it to propagate the child's exit code to its own process exit.
    /// `None` while the child is still running (or if the child handle is
    /// poisoned).
    ///
    /// The vendored portable-pty decodes Unix signal death into the shell's
    /// `128 + signo`, so SIGTERM is `143` and SIGKILL is `137`. Agent
    /// automation driving `kettle exec` relies on this to tell a killed command
    /// from one that merely failed. The numeric signal is retained alongside
    /// its name if a caller needs to act on it directly. Callers clamp to
    /// 0..=255 on Unix before `std::process::exit` regardless.
    pub fn child_exit_code(&self) -> Option<u32> {
        self.child
            .lock()
            .ok()
            .and_then(|mut c| c.try_wait().ok().flatten())
            .map(|st| st.exit_code())
    }

    /// A plain-text snapshot of the grid.
    /// Without extra scrollback, this is the visible viewport; with
    /// `scrollback_lines`, it returns up to that many of the newest history
    /// rows (oldest first) followed by the active screen for command-output
    /// capture. One lock acquisition; per-line trailing whitespace trimmed;
    /// `scrollback_lines` hard-capped at 10_000 so a hostile/buggy caller can't
    /// ask for an unbounded join. This is the single sanctioned grid scrape,
    /// shared by the control server's `read_screen`, `run_command` output
    /// slicing, and any future scripting surface.
    pub fn screen_text(&self, scrollback_lines: usize) -> Option<ScreenText> {
        let t = self.term.lock().ok()?;
        Some(screen_text_of(&t, scrollback_lines))
    }
}

/// The selection could not be read within the limit asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SelectionTextTooLarge;

/// The selected text, or `None` with nothing selected, when it fits in
/// `cap` bytes. The cells are counted first, a conservative bound with
/// combining marks and wide characters' cells, so a huge selection is
/// refused before the engine builds its string.
pub fn selection_text_bounded(
    term: &Term<EventProxy>,
    cap: usize,
) -> Result<Option<String>, SelectionTextTooLarge> {
    let Some(range) = term
        .selection
        .as_ref()
        .and_then(|selection| selection.to_range(term))
    else {
        return Ok(None);
    };
    cells_within(term, &range, cap)?;
    let text = term.selection_to_string();
    if text.as_ref().is_some_and(|text| text.len() > cap) {
        return Err(SelectionTextTooLarge);
    }
    Ok(text)
}

/// Whether the text of `range`'s cells is sure to fit in `cap` bytes, by a
/// conservative count of each cell's character, its combining marks, and a
/// line break and what a wrapped wide character adds per line.
fn cells_within(
    term: &Term<EventProxy>,
    range: &alacritty_terminal::selection::SelectionRange,
    cap: usize,
) -> Result<(), SelectionTextTooLarge> {
    use alacritty_terminal::index::{Column, Line, Point};
    let columns = term.columns();
    if columns == 0 {
        return Err(SelectionTextTooLarge);
    }
    let mut bytes = 0usize;
    let mut add = |more: usize| {
        bytes = bytes.saturating_add(more);
        if bytes > cap {
            Err(SelectionTextTooLarge)
        } else {
            Ok(())
        }
    };
    for line in range.start.line.0..=range.end.line.0 {
        // A line break, and what a wrapped wide character adds.
        add(8)?;
        let first = if range.is_block || line == range.start.line.0 {
            range.start.column.0
        } else {
            0
        }
        .saturating_sub(1)
        .min(columns - 1);
        let last = if range.is_block || line == range.end.line.0 {
            range.end.column.0
        } else {
            columns - 1
        }
        .min(columns - 1);
        for column in first..=last {
            let cell = &term.grid()[Point::new(Line(line), Column(column))];
            add(cell.c.len_utf8())?;
            for mark in cell.zerowidth().into_iter().flatten() {
                add(mark.len_utf8())?;
            }
        }
    }
    Ok(())
}

/// The Mermaid sources in the selection, or `None` with nothing selected,
/// joined back across hard wraps at the terminal's width. A selection
/// that starts inside a row leaves that row's leading columns out; they
/// are put back as spaces, so the first row keeps its indentation against
/// the rows below. A rectangle cuts every row at its sides, losing the
/// indentation and the ends a diagram's rows need, so the lines it spans
/// are taken whole.
pub fn selected_diagram_sources(
    term: &Term<EventProxy>,
) -> Result<Option<Vec<String>>, crate::DiagramCopyError> {
    let Some(range) = term
        .selection
        .as_ref()
        .and_then(|selection| selection.to_range(term))
    else {
        return Ok(None);
    };
    let too_large = |_| crate::DiagramCopyError::TooLarge;
    let text = if range.is_block {
        use alacritty_terminal::index::{Column, Point};
        let lines = alacritty_terminal::selection::SelectionRange::new(
            Point::new(range.start.line, Column(0)),
            Point::new(range.end.line, term.last_column()),
            false,
        );
        cells_within(term, &lines, crate::MAX_DIAGRAM_COPY_BYTES).map_err(too_large)?;
        term.bounds_to_string(lines.start, lines.end)
    } else {
        let Some(text) =
            selection_text_bounded(term, crate::MAX_DIAGRAM_COPY_BYTES).map_err(too_large)?
        else {
            return Ok(None);
        };
        text
    };
    let text = if range.is_block || range.start.column.0 == 0 {
        text
    } else {
        let padding = range.start.column.0;
        if padding.saturating_add(text.len()) > crate::MAX_DIAGRAM_COPY_BYTES {
            return Err(crate::DiagramCopyError::TooLarge);
        }
        format!("{:padding$}{text}", "")
    };
    crate::diagram_sources(&text, Some(term.columns())).map(Some)
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
        // every wide (CJK/emoji) glyph.
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
        // DEC ?25 visibility. vim/fzf/less hide the cursor, and an agent
        // placing keystrokes by cursor position needs to know when the
        // reported point is meaningless.
        cursor_visible: t
            .mode()
            .contains(alacritty_terminal::term::TermMode::SHOW_CURSOR),
    }
}

/// The result of [`Terminal::screen_text`], holding the joined plain text and
/// the grid geometry an agent needs to interpret it.
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
    /// Whether the cursor is shown (DEC ?25; vim/fzf/less hide it).
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

#[cfg(unix)]
fn terminate_unreaped_child(child: &mut dyn portable_pty::Child) -> std::io::Result<()> {
    if child.try_wait()?.is_none() {
        child.kill()?;
    }
    Ok(())
}

impl Drop for Terminal {
    /// Tear down the PTY WITHOUT ever blocking the calling thread.
    ///
    /// This runs on the UI thread — closing a pane drops the owned
    /// `Pane.term` (`Mux::close_focused` → `panes.remove`) — so blocking here
    /// freezes the whole window.
    ///
    /// Drop must never `join()` the reader thread. The reader blocks in
    /// `read()` on the ConPTY conout pipe until the pseudoconsole is *closed*,
    /// so joining it before that close deadlocks the UI thread. Windows then
    /// marks the window "not responding", which users see as a crash.
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
        #[cfg(unix)]
        if let Some(mut trace) = self.startup_geometry.take() {
            trace.finish(self.master.as_ref().and_then(|m| m.as_raw_fd()), false);
        }
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
                    #[cfg(unix)]
                    let result = terminate_unreaped_child(child.as_mut());
                    #[cfg(not(unix))]
                    let result = child.kill();
                    if let Err(error) = result {
                        log::warn!("could not terminate PTY child: {error}");
                    }
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

/// A cloneable, thread-safe handle to a PTY's write side, obtained from
/// [`Terminal::writer_handle`]. Lets a worker thread feed input to the child
/// without sharing the (non-`Sync`) `Terminal`.
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
mod complete_pty_write_tests;

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
mod pty_stdin_lease_phase_tests;

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
            if written != 0 {
                self.pending_eof = None;
            }
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
                let Some(_) = pty_eof_sequence(
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
            let unterminated_record = self
                .input_state
                .record_unterminated(pending.rules)
                .map_err(anyhow::Error::msg)?;
            let written = self.writer.write_some_checked(&[pending.configured_veof])?;
            if written == 0 {
                return Ok(PtyEofProgress::Pending);
            }
            self.input_state
                .observe(&[pending.configured_veof], pending.rules);
            if unterminated_record {
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
    /// The whole body is dropped. It can hold Sixel pixel data or Kitty
    /// graphics payloads, which carry encoded file paths and shared-memory
    /// names and must not reach the session log as text.
    String,
}

/// Longest unterminated control sequence the log stripper drops before it
/// resynchronizes to plain text.
///
/// Every terminator the state machine honors is one the writer chooses to
/// send. A child that simply never sends one would otherwise suppress the
/// whole remaining log. `kettle exec`'s stripper draws the same line at the
/// same place.
const MAX_LOGGED_CONTROL_SEQUENCE_BYTES: usize = 64 * 1024;

/// Terminator parity (`plugins/logger.py` extension): a persistent-state
/// stripper for ANSI/OSC escape sequences, used by the per-pane session-log
/// path (`log_strip_ansi`). Recognizes:
///   - CSI (Control Sequence Introducer): `ESC [ params final`
///     where final is in `0x40..=0x7e`.
///   - OSC (Operating System Command): `ESC ] ... terminator`
///     where terminator is BEL (0x07) or ST (`ESC \\`).
///   - DCS/SOS/PM/APC strings: `ESC P`, `ESC X`, `ESC ^`, or `ESC _`, with
///     the body dropped up to ST.
///   - Other escapes: `ESC`, any `0x20..=0x2f` intermediates, then one final
///     byte.
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
    /// `0x9c` is both the 8-bit ST and a UTF-8 continuation byte. Without this
    /// count, a payload containing `末` (`e6 9c ab`) would end the control
    /// string mid-character and leak the rest into the log. The VT extractor
    /// draws the same distinction.
    utf8_continuation: u8,
    /// Whether the lead byte of the current UTF-8 scalar reached the log.
    /// Continuations follow their lead across parser-state transitions.
    utf8_emitted: bool,
    /// Bytes consumed since the `ESC` that opened the current sequence.
    ///
    /// Counted in every non-plain state, reset when a fresh `ESC` arrives, and
    /// compared against [`MAX_LOGGED_CONTROL_SEQUENCE_BYTES`].
    control_bytes: usize,
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
                    if !matches!(self.state, StripState::Plain) {
                        self.control_bytes += 1;
                        if self.control_bytes > MAX_LOGGED_CONTROL_SEQUENCE_BYTES {
                            self.state = StripState::Plain;
                        }
                    }
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
            if b == 0x1b {
                self.control_bytes = 0;
            } else if !matches!(self.state, StripState::Plain) {
                self.control_bytes += 1;
                if self.control_bytes > MAX_LOGGED_CONTROL_SEQUENCE_BYTES {
                    // Give up on the terminator and resume logging. The byte
                    // that tripped the bound is dropped with the sequence it
                    // belongs to; `utf8_emitted` is already false, so if it was
                    // a UTF-8 lead its continuations go with it rather than
                    // reaching the log alone.
                    self.state = StripState::Plain;
                    continue;
                }
            }
            match self.state {
                StripState::Plain => {
                    if b == 0x1b {
                        self.state = StripState::EscSeen;
                        self.control_bytes = 0;
                    } else {
                        out.push(b);
                    }
                }
                StripState::EscSeen => {
                    if matches!(b, 0x00..=0x17 | 0x19 | 0x1c..=0x1f) {
                        out.push(b);
                    }
                    self.state = Self::escape_follower(b);
                }
                StripState::EscapeIntermediate => {
                    if matches!(b, 0x00..=0x17 | 0x19 | 0x1c..=0x1f) {
                        out.push(b);
                    }
                    if b == 0x1b {
                        self.state = StripState::EscSeen;
                    } else if b == 0x18 || b == 0x1a || (0x30..=0x7e).contains(&b) {
                        self.state = StripState::Plain;
                    }
                }
                StripState::Csi => {
                    if matches!(b, 0x00..=0x17 | 0x19 | 0x1c..=0x1f) {
                        out.push(b);
                    }
                    if b == 0x1b {
                        self.state = StripState::EscSeen;
                    } else if b == 0x18 || b == 0x1a {
                        // CAN/SUB cancel the sequence. Otherwise the next
                        // ordinary character is eaten as the CSI final byte,
                        // and `ESC [ 31 CAN hello` logs `ello` while the
                        // terminal renders `hello`.
                        self.state = StripState::Plain;
                    } else if (0x40..=0x7e).contains(&b) {
                        self.state = StripState::Plain;
                    }
                    // Else still inside CSI params — keep scanning.
                }
                StripState::Osc => {
                    if b == 0x07 || b == 0x9c {
                        self.state = StripState::Plain; // BEL or 8-bit ST
                    } else if b == 0x18 || b == 0x1a {
                        // CAN/SUB cancel the string (DEC). Otherwise the text
                        // after a cancelled OSC is dropped from the log.
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
            0x00..=0x17 | 0x19 | 0x1b..=0x1f | 0x7f => StripState::EscSeen,
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
    inactive: &'a InactiveGraphics,
    images: &'a Images,
    virtuals: &'a Virtuals,
    anims: &'a Animations,
    relatives: &'a Relatives,
    geometry: &'a Arc<Mutex<VersionedPtyGeometry>>,
    proxy: &'a EventProxy,
}

fn relative_resolver<'a>(
    images: &Images,
    virtuals: &Virtuals,
    cells: &[(u64, usize, placeholder::ResolvedCell)],
    relatives: impl IntoIterator<Item = ((u32, u32), &'a RelEntry)>,
) -> RelativeResolver {
    relative_resolver_excluding(images, virtuals, cells, relatives, None)
}

fn relative_resolver_excluding<'a>(
    images: &Images,
    virtuals: &Virtuals,
    cells: &[(u64, usize, placeholder::ResolvedCell)],
    relatives: impl IntoIterator<Item = ((u32, u32), &'a RelEntry)>,
    retiring: Option<PlacementKey>,
) -> RelativeResolver {
    let mut relatives = relatives.into_iter().peekable();
    if relatives.peek().is_none() {
        return RelativeResolver::default();
    }
    let physical: Vec<_> = images
        .lock()
        .map(|placements| {
            placements
                .iter()
                .filter_map(|placement| {
                    Some((
                        PlacementKey {
                            image_id: placement.id?,
                            placement_id: placement.placement_id,
                        },
                        (placement.abs_line, placement.col),
                    ))
                })
                .filter(|(key, _)| Some(*key) != retiring)
                .collect()
        })
        .unwrap_or_default();
    let virtual_keys: Vec<_> = virtuals
        .lock()
        .map(|placements| {
            placements
                .keys()
                .map(|&(image_id, placement_id)| PlacementKey {
                    image_id,
                    placement_id,
                })
                .filter(|key| Some(*key) != retiring)
                .collect()
        })
        .unwrap_or_default();
    RelativeResolver::new(
        physical,
        virtual_keys,
        cells.iter().copied(),
        relatives.map(|((image_id, placement_id), entry)| {
            (
                PlacementKey {
                    image_id,
                    placement_id,
                },
                PlacementKey {
                    image_id: entry.parent_img,
                    placement_id: entry.parent_placement,
                },
                entry.h,
                entry.v,
            )
        }),
    )
}

fn apply_kitty_delete_at(
    term: &Term<EventProxy>,
    delete: KittyDelete,
    context: GraphicsActionContext<'_>,
    extractor: &mut Extractor,
) {
    let relative_snapshot = context
        .relatives
        .lock()
        .map(|placements| placements.clone())
        .unwrap_or_default();
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
    let placeholder_cells = if relative_snapshot.is_empty() {
        Vec::new()
    } else {
        Terminal::placeholder_cells_from_term(term)
    };
    let render_geometry = context
        .geometry
        .lock()
        .map(|geometry| geometry.geometry)
        .unwrap_or_else(|_| PtyGeometry::new(1, 1, 1, 1));

    // Resolve relative-placement origins before mutating any registry.
    let resolver = relative_resolver(
        context.images,
        context.virtuals,
        &placeholder_cells,
        relative_snapshot.iter().map(|(&key, entry)| (key, entry)),
    );
    let relative_positions = relative_snapshot
        .iter()
        .filter_map(|(&(id, placement_id), entry)| {
            let (parent_abs, parent_col) = resolver.resolve(
                PlacementKey {
                    image_id: entry.parent_img,
                    placement_id: entry.parent_placement,
                },
                8,
            )?;
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
        relative_placements.retain(|&(image_id, placement_id), entry| {
            let matched = matches!(delete.target, KittyDeleteTarget::ZIndex { z } if entry.z == z)
                || relative_positions
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

        cascade_removed_relatives(
            &mut relative_placements,
            &mut removed_keys,
            &mut removed_ids,
        );
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

// Callers hold the graphics gate across this snapshot and its placement bases.
// Keep registry locks separate so rendering retains the existing lock order.
fn apply_animation_snapshot(
    id: u32,
    imgs: Vec<crate::ImageData>,
    gaps: Vec<i32>,
    state: kettle_vt::kitty::AnimationState,
    context: GraphicsActionContext<'_>,
) {
    let root = imgs.first().cloned();
    {
        let Ok(mut animations) = context.anims.lock() else {
            return;
        };
        if imgs.len() <= 1 && !state.running {
            animations.remove(&id);
        } else {
            let limits = kettle_vt::GraphicsLimits::default();
            let bytes = imgs
                .iter()
                .try_fold(0usize, |bytes, image| bytes.checked_add(image.byte_len()));
            if !(animations.contains_key(&id) || animations.len() < limits.placements)
                || imgs.len() > limits.animation_frames.saturating_add(1)
                || !limits
                    .animation_bytes
                    .checked_add(limits.image_bytes)
                    .zip(bytes)
                    .is_some_and(|(cap, bytes)| bytes <= cap)
            {
                return;
            }
            let started = match animations.get(&id) {
                Some(previous) if previous.state.running == state.running => previous.started,
                _ => std::time::Instant::now(),
            };
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
    let Some(root) = root else {
        return;
    };
    if let Ok(mut placements) = context.images.lock() {
        for placement in placements
            .iter_mut()
            .filter(|placement| placement.id == Some(id))
        {
            placement.img = root.clone();
        }
    }
    if let Ok(mut virtuals) = context.virtuals.lock() {
        for (&(image_id, _), placement) in virtuals.iter_mut() {
            if image_id == id {
                placement.img = root.clone();
            }
        }
    }
    if let Ok(mut relatives) = context.relatives.lock() {
        for (&(image_id, _), placement) in relatives.iter_mut() {
            if image_id == id {
                placement.img = root.clone();
            }
        }
    }
}

fn quota_key(
    inactive: bool,
    id: Option<u32>,
    image: &crate::ImageData,
) -> Option<kettle_vt::quota::QuotaKey> {
    use kettle_vt::quota::{QuotaIdentity, QuotaKey};
    let image = match id {
        Some(id) => QuotaIdentity::Named(id),
        None if image.kitty_creation_order().is_some() => {
            QuotaIdentity::Anonymous(image.allocation_key())
        }
        None => return None,
    };
    Some(QuotaKey { inactive, image })
}

fn note_quota_placements(
    owners: &mut Vec<kettle_vt::quota::QuotaOwner>,
    inactive: bool,
    placements: &[Placement],
) {
    for placement in placements {
        if let Some(key) = quota_key(inactive, placement.id, &placement.img) {
            owners.push(kettle_vt::quota::QuotaOwner::new(key, &placement.img, true));
        }
    }
}

fn note_quota_virtuals(
    owners: &mut Vec<kettle_vt::quota::QuotaOwner>,
    inactive: bool,
    virtuals: &std::collections::HashMap<(u32, u32), VirtualEntry>,
) {
    for (&(id, _), entry) in virtuals {
        if let Some(key) = quota_key(inactive, Some(id), &entry.img) {
            owners.push(kettle_vt::quota::QuotaOwner::new(key, &entry.img, true));
        }
    }
}

fn note_quota_relatives(
    owners: &mut Vec<kettle_vt::quota::QuotaOwner>,
    inactive: bool,
    relatives: &std::collections::HashMap<(u32, u32), RelEntry>,
) {
    for (&(id, _), entry) in relatives {
        if let Some(key) = quota_key(inactive, Some(id), &entry.img) {
            owners.push(kettle_vt::quota::QuotaOwner::new(key, &entry.img, true));
        }
    }
}

fn note_quota_animations(
    owners: &mut Vec<kettle_vt::quota::QuotaOwner>,
    inactive: bool,
    animations: &std::collections::HashMap<u32, AnimEntry>,
) {
    for (&id, entry) in animations {
        for image in &entry.imgs {
            if let Some(key) = quota_key(inactive, Some(id), image) {
                owners.push(kettle_vt::quota::QuotaOwner::new(key, image, false));
            }
        }
    }
}

fn remove_quota_placements(
    placements: &mut Vec<Placement>,
    inactive: bool,
    victims: &std::collections::HashSet<kettle_vt::quota::QuotaKey>,
    removed: &mut std::collections::HashSet<PlacementKey>,
) {
    placements.retain(|placement| {
        if quota_key(inactive, placement.id, &placement.img)
            .is_some_and(|key| victims.contains(&key))
        {
            if let Some(image_id) = placement.id {
                removed.insert(PlacementKey {
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

fn remove_quota_virtuals(
    virtuals: &mut std::collections::HashMap<(u32, u32), VirtualEntry>,
    ids: &std::collections::HashSet<u32>,
    removed: &mut std::collections::HashSet<PlacementKey>,
) {
    virtuals.retain(|&(image_id, placement_id), _| {
        if ids.contains(&image_id) {
            removed.insert(PlacementKey {
                image_id,
                placement_id,
            });
            false
        } else {
            true
        }
    });
}

fn remove_quota_relatives(
    relatives: &mut std::collections::HashMap<(u32, u32), RelEntry>,
    ids: &std::collections::HashSet<u32>,
    mut removed: std::collections::HashSet<PlacementKey>,
) {
    removed.extend(
        relatives
            .iter()
            .filter_map(|(&(image_id, placement_id), entry)| {
                (ids.contains(&image_id) || ids.contains(&entry.parent_img)).then_some(
                    PlacementKey {
                        image_id,
                        placement_id,
                    },
                )
            }),
    );
    let closure = kettle_vt::kitty::relative_deletion_closure(
        relatives.iter().map(|(&(image_id, placement_id), entry)| {
            (
                PlacementKey {
                    image_id,
                    placement_id,
                },
                PlacementKey {
                    image_id: entry.parent_img,
                    placement_id: entry.parent_placement,
                },
            )
        }),
        removed,
    );
    relatives.retain(|&(image_id, placement_id), _| {
        !closure.contains(&PlacementKey {
            image_id,
            placement_id,
        })
    });
}

fn apply_image_quota(
    request: kettle_vt::quota::QuotaRequest,
    context: GraphicsActionContext<'_>,
    extractor: &mut Extractor,
) {
    // Preserve registry ownership through planning and removal, in the existing
    // images -> virtuals -> anims -> relatives order. Render snapshots cannot
    // appear between the live allocation count and eviction; no Term lock is
    // acquired here.
    let (
        Ok(mut placements),
        Ok(mut virtuals),
        Ok(mut animations),
        Ok(mut relatives),
        Ok(mut inactive),
    ) = (
        context.images.lock(),
        context.virtuals.lock(),
        context.anims.lock(),
        context.relatives.lock(),
        context.inactive.lock(),
    )
    else {
        return;
    };
    let mut owners = Vec::new();
    note_quota_placements(&mut owners, false, &placements);
    note_quota_virtuals(&mut owners, false, &virtuals);
    note_quota_animations(&mut owners, false, &animations);
    note_quota_relatives(&mut owners, false, &relatives);
    note_quota_placements(&mut owners, true, &inactive.placements);
    note_quota_virtuals(&mut owners, true, &inactive.virtuals);
    note_quota_animations(&mut owners, true, &inactive.animations);
    note_quota_relatives(&mut owners, true, &inactive.relatives);
    let Some(plan) = request.plan(&owners) else {
        return;
    };
    let victims: std::collections::HashSet<_> = plan.iter().copied().collect();
    let ids = |inactive| {
        plan.iter()
            .filter_map(|key| match key.image {
                kettle_vt::quota::QuotaIdentity::Named(id) if key.inactive == inactive => Some(id),
                _ => None,
            })
            .collect::<std::collections::HashSet<_>>()
    };
    let active_ids = ids(false);
    let mut removed = std::collections::HashSet::new();
    remove_quota_placements(&mut placements, false, &victims, &mut removed);
    remove_quota_virtuals(&mut virtuals, &active_ids, &mut removed);
    remove_quota_relatives(&mut relatives, &active_ids, removed);
    animations.retain(|id, _| !active_ids.contains(id));
    let inactive_ids = ids(true);
    let mut removed = std::collections::HashSet::new();
    remove_quota_placements(&mut inactive.placements, true, &victims, &mut removed);
    remove_quota_virtuals(&mut inactive.virtuals, &inactive_ids, &mut removed);
    remove_quota_relatives(&mut inactive.relatives, &inactive_ids, removed);
    inactive
        .animations
        .retain(|id, _| !inactive_ids.contains(id));
    extractor.apply_image_quota(&plan);
}

#[derive(Clone, Copy, Debug)]
enum GraphicsAdmissionError {
    InvalidGeometry,
    Unavailable,
    MissingParent,
}

fn finish_graphics_placement(
    extractor: &mut Extractor,
    proxy: &EventProxy,
    completion: Option<kettle_vt::kitty::PlacementCompletion>,
    result: Result<(), GraphicsAdmissionError>,
) {
    let Some(completion) = completion else {
        return;
    };
    let status = match result {
        Ok(()) => kettle_vt::kitty::CommandStatus::Ok,
        Err(GraphicsAdmissionError::InvalidGeometry) => {
            kettle_vt::kitty::CommandStatus::InvalidPlacement
        }
        Err(GraphicsAdmissionError::Unavailable) => kettle_vt::kitty::CommandStatus::NoSpace,
        Err(GraphicsAdmissionError::MissingParent) => {
            kettle_vt::kitty::CommandStatus::MissingParent
        }
    };
    if let Some(reply) = extractor.finish_kitty_placement(completion, status) {
        send_graphics_reply(proxy, reply);
    }
}

enum PlacementForm {
    Physical,
    Virtual,
    Relative,
}

fn reconcile_named_placement(
    images: &Images,
    virtuals: &Virtuals,
    relatives: &Relatives,
    key: Option<(u32, u32)>,
    form: PlacementForm,
) {
    let Some((id, placement)) = key.filter(|&(_, placement)| placement != 0) else {
        return;
    };
    // Admission has committed. Cleanup cannot turn it into a rejected update.
    if !matches!(form, PlacementForm::Physical) {
        images
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .retain(|entry| entry.id != Some(id) || entry.placement_id != placement);
    }
    if !matches!(form, PlacementForm::Virtual) {
        virtuals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&(id, placement));
    }
    if !matches!(form, PlacementForm::Relative) {
        relatives
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&(id, placement));
    }
}

fn contains_placement_parent<T>(placements: &HashMap<(u32, u32), T>, parent: PlacementKey) -> bool {
    if parent.placement_id != 0 {
        placements.contains_key(&(parent.image_id, parent.placement_id))
    } else {
        placements.keys().any(|&(id, _)| id == parent.image_id)
    }
}

fn has_placement_parent(
    context: GraphicsActionContext<'_>,
    parent: PlacementKey,
) -> Result<bool, GraphicsAdmissionError> {
    if context
        .images
        .lock()
        .map_err(|_| GraphicsAdmissionError::Unavailable)?
        .iter()
        .any(|placement| {
            placement.id == Some(parent.image_id)
                && (parent.placement_id == 0 || placement.placement_id == parent.placement_id)
        })
    {
        return Ok(true);
    }
    {
        let virtuals = context
            .virtuals
            .lock()
            .map_err(|_| GraphicsAdmissionError::Unavailable)?;
        if contains_placement_parent(&virtuals, parent) {
            return Ok(true);
        }
    }
    let relatives = context
        .relatives
        .lock()
        .map_err(|_| GraphicsAdmissionError::Unavailable)?;
    Ok(contains_placement_parent(&relatives, parent))
}

fn apply_graphics_chunk_at(
    term: &mut Term<EventProxy>,
    chunk: Chunk,
    context: GraphicsActionContext<'_>,
    extractor: &mut Extractor,
) -> bool {
    match chunk {
        Chunk::GraphicsReply(reply) => send_graphics_reply(context.proxy, reply),
        Chunk::Image(mut placed) => {
            let key = placed.id.map(|id| (id, placed.placement_id));
            let completion = placed.completion.take();
            let geometry = context
                .geometry
                .lock()
                .map(|geometry| geometry.geometry)
                .unwrap_or_else(|_| PtyGeometry::new(1, 1, 1, 1));
            let result = place_image_during_sync(term, context.images, geometry, placed);
            if result.is_ok() {
                reconcile_named_placement(
                    context.images,
                    context.virtuals,
                    context.relatives,
                    key,
                    PlacementForm::Physical,
                );
            }
            finish_graphics_placement(extractor, context.proxy, completion, result);
        }
        Chunk::DeleteImages(delete) => apply_kitty_delete_at(term, delete, context, extractor),
        Chunk::ImageQuota(request) => apply_image_quota(request, context, extractor),
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
            completion,
        } => {
            let result = (|| {
                if !has_placement_parent(
                    context,
                    PlacementKey {
                        image_id: parent_img,
                        placement_id: parent_placement,
                    },
                )? {
                    return Err(GraphicsAdmissionError::MissingParent);
                }
                let geometry = context
                    .geometry
                    .lock()
                    .map_err(|_| GraphicsAdmissionError::Unavailable)?
                    .geometry;
                resolve_kitty_placement(&img, params, geometry)
                    .ok_or(GraphicsAdmissionError::InvalidGeometry)?;
                let key = (id, placement);
                let entry = RelEntry {
                    img,
                    parent_img,
                    parent_placement,
                    h,
                    v,
                    z,
                    params,
                };
                let mut proposed = context
                    .relatives
                    .lock()
                    .map_err(|_| GraphicsAdmissionError::Unavailable)?
                    .clone();
                proposed.insert(key, entry.clone());
                let resolver = relative_resolver_excluding(
                    context.images,
                    context.virtuals,
                    &Terminal::placeholder_cells_from_term(term),
                    proposed.iter().map(|(&key, entry)| (key, entry)),
                    (placement != 0).then_some(PlacementKey {
                        image_id: id,
                        placement_id: placement,
                    }),
                );
                if resolver.has_cycle(PlacementKey {
                    image_id: id,
                    placement_id: placement,
                }) {
                    return Err(GraphicsAdmissionError::InvalidGeometry);
                }
                let mut relatives = context
                    .relatives
                    .lock()
                    .map_err(|_| GraphicsAdmissionError::Unavailable)?;
                let limit = kettle_vt::GraphicsLimits::default().placements;
                if !relatives.contains_key(&key) && relatives.len() >= limit {
                    return Err(GraphicsAdmissionError::Unavailable);
                }
                relatives.insert(key, entry);
                Ok(())
            })();
            if result.is_ok() {
                reconcile_named_placement(
                    context.images,
                    context.virtuals,
                    context.relatives,
                    Some((id, placement)),
                    PlacementForm::Relative,
                );
            }
            finish_graphics_placement(extractor, context.proxy, completion, result);
        }
        Chunk::VirtualImage {
            id,
            placement,
            img,
            cols,
            rows,
            z,
            completion,
        } => {
            let result = (|| {
                let mut virtuals = context
                    .virtuals
                    .lock()
                    .map_err(|_| GraphicsAdmissionError::Unavailable)?;
                let key = (id, placement);
                let limit = kettle_vt::GraphicsLimits::default().placements;
                if !virtuals.contains_key(&key) && virtuals.len() >= limit {
                    return Err(GraphicsAdmissionError::Unavailable);
                }
                virtuals.insert(
                    key,
                    VirtualEntry {
                        img,
                        placement_id: placement,
                        cols,
                        rows,
                        z,
                    },
                );
                Ok(())
            })();
            if result.is_ok() {
                reconcile_named_placement(
                    context.images,
                    context.virtuals,
                    context.relatives,
                    Some((id, placement)),
                    PlacementForm::Virtual,
                );
            }
            finish_graphics_placement(extractor, context.proxy, completion, result);
        }
        Chunk::Animation {
            id,
            imgs,
            gaps,
            state,
        } => {
            apply_animation_snapshot(id, imgs, gaps, state, context);
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
) -> Result<(Option<PlacementParams>, usize, usize), GraphicsAdmissionError> {
    let kettle_vt::Placed {
        img: data,
        id,
        placement_id,
        z,
        params,
        completion: _,
    } = placed;
    let resolved = if let Some(params) = params {
        resolve_kitty_placement(&data, params, geometry)
            .ok_or(GraphicsAdmissionError::InvalidGeometry)?
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
    {
        let mut placements = images
            .lock()
            .map_err(|_| GraphicsAdmissionError::Unavailable)?;
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
    Ok((params, cell_cols, cell_rows))
}

fn place_image_during_sync(
    term: &mut Term<EventProxy>,
    images: &Images,
    geometry: PtyGeometry,
    placed: kettle_vt::Placed,
) -> Result<(), GraphicsAdmissionError> {
    let (params, cell_cols, cell_rows) = insert_image_at_cursor(term, images, geometry, placed)?;
    if let Some(params) = params {
        if !params.suppress_cursor_movement {
            let (right, down) = kitty_cursor_counts(cell_cols, cell_rows);
            term.move_forward(right);
            term.move_down(down);
        }
    } else {
        for _ in 0..cell_rows.clamp(1, 256) {
            term.carriage_return();
            term.linefeed();
        }
    }
    Ok(())
}

fn place_image(
    term: &SharedTerm,
    images: &Images,
    geometry: &Arc<Mutex<VersionedPtyGeometry>>,
    processor: &mut Processor,
    placed: kettle_vt::Placed,
) -> Result<Option<GraphicsEventBatch>, GraphicsAdmissionError> {
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
    Ok(advanced.then(|| t.take_graphics_events()))
}

#[cfg(test)]
mod cwd_reporting_tests;

/// The PTY reader acquires `term` then `virtuals`; render code must never take
/// them in the opposite order. `virtuals_snapshot` returns owned maps, so its
/// guard cannot survive into `placeholder_cells`, which acquires `term`.
///
/// The source guard below pins that call path and owned signature. It cannot
/// detect a future indirect lock hidden behind a macro or another helper.
#[cfg(test)]
mod placeholder_lock_order_tests;

#[cfg(test)]
mod placeholder_tile_placement_tests;

/// Tests for new-tab shell detection, covering WSL distros, the Windows and
/// Unix dropdown builders, Visual Studio dev shells, and Git Bash.
#[cfg(test)]
mod detect_shells_tests;

#[cfg(test)]
mod wslenv_tests;

#[cfg(test)]
mod working_directory_tests;

#[cfg(test)]
mod home_dir_tests;

#[cfg(test)]
mod conformance;

#[cfg(test)]
mod teardown_tests;

#[cfg(test)]
mod login_flag_tests;

#[cfg(all(test, windows))]
mod default_shell_tests {
    use super::{
        POWERSHELL_BOOTSTRAP_GROWTH_HEADROOM, POWERSHELL_BOOTSTRAP_MAX_CHARS,
        POWERSHELL_BOOTSTRAP_PREFIX, POWERSHELL_BOOTSTRAP_SUFFIX, POWERSHELL_INTEGRATION,
        base64_standard, is_powershell, merge_windows_paths, overlay_windows_parent_env,
        pick_windows_default_shell, powershell_integration_bootstrap,
        powershell_integration_command,
    };
    use portable_pty::CommandBuilder;
    use std::ffi::{OsStr, OsString};
    use std::path::{Path, PathBuf};

    const PWSH: &str = r"C:\Program Files\PowerShell\7\pwsh.exe";
    const WPS: &str = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";

    /// The self-contained base64 encoder matches known vectors (RFC 4648).
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
        assert!(
            bootstrap.len() + POWERSHELL_BOOTSTRAP_GROWTH_HEADROOM
                <= POWERSHELL_BOOTSTRAP_MAX_CHARS,
            "the unmodified integration needs useful bootstrap headroom"
        );

        let command = powershell_integration_command(Path::new(PWSH));
        let argv = command.get_argv();
        assert_eq!(argv[1], "-NoExit");
        assert_eq!(argv[2], "-Command");
        assert_eq!(argv[3], OsStr::new(&bootstrap));
    }

    /// PowerShell executables are recognized by basename; cmd / bash are not.
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
mod wsl_launcher_tests;

#[cfg(test)]
mod pty_dim_tests;

#[cfg(test)]
mod kitty_placement_geometry_tests;

#[cfg(test)]
mod atomic_geometry_tests;

#[cfg(test)]
mod output_publish_guard;

#[cfg(test)]
mod output_sender_tests;

#[cfg(test)]
mod pty_read_status_tests;

#[cfg(test)]
mod sync_update_flush_guard;

#[cfg(test)]
mod image_lifecycle_tests;

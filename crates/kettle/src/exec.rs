//! `kettle exec` (agent-first A1) — run a command under a real
//! PTY with full VT emulation, headlessly (no GPU, no window, no winit), and
//! stream its output to this process's real stdout.
//!
//! This is the non-interactive half of "agents work great with kettle". An AI
//! agent (or any script) gets: a real TTY for the child (so `ls --color`,
//! `python -c`, `git` etc. behave exactly as in a terminal), verbatim output
//! on a real pipe (`kettle exec -- … | grep`), and the child's true exit code
//! propagated as kettle's own. The interactive half is the GUI; the
//! programmatic-control half is `kettle ctl` / `kettle mcp`.
//!
//! Design notes:
//!   - The engine is `kettle_core::Terminal`, which is fully GUI-decoupled: it
//!     owns the PTY + the alacritty VT state machine and ships verbatim output
//!     bytes on a sidechannel. We drive it with a no-op waker, a lossless
//!     bounded output queue, and a lossless event channel. The output queue
//!     backpressures a fast child when stdout is slow instead of growing heap.
//!   - We MUST forward `TermEvent::PtyWrite` (and the other reply-bearing
//!     events) back to the PTY. Those are the child's terminal-query answers
//!     (DA1, DSR cursor-position, text-area size, OSC color queries). Without
//!     them a TUI hangs — and on Windows the ConPTY layer withholds the
//!     child's clean teardown until its startup `ESC[6n` is answered, so
//!     `try_wait` never reports the exit. This is the load-bearing detail.
//!   - On Windows the GUI subsystem is no obstacle: `main` already runs
//!     `attach_parent_console_if_needed()`, which leaves an inherited stdout
//!     pipe untouched, so `kettle exec` under Claude Code / a shell pipe gets a
//!     real pipe on all three std handles.
//!
//! This module is bin-side and deliberately has NO `kettle_ui` / winit
//! dependency — a source-scan drift guard pins that ("headless means
//! headless"). It reuses `kettle_render`'s pure VT-reply helpers only.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use kettle_core::{
    CursorShape, PtyEofProgress, PtyGeometry, PtyOutputSender, PtyReadProgress, PtyReadStatus,
    PtyStdin, TermEvent, Terminal, TerminalCapabilities, Waker, WorkingDirectoryPolicy,
};

/// How long to keep draining output after the child exits before we stop and
/// report the code. Doubles as the ConPTY late-repaint mitigation: ConPTY's
/// screen-differ can emit a final paint after the child is gone. Same order of
/// magnitude as the dev-record reap settle.
const SETTLE: Duration = Duration::from_millis(60);
/// How long an exit event may lead the authoritative child status. Keep the
/// lifecycle loop polling during this window so cancellation and deadlines
/// remain enforceable.
const CHILD_EXIT_STATUS_WAIT: Duration = Duration::from_millis(250);
/// How long ConPTY may keep its reader alive after the child is gone before
/// ordinary completion closes the pseudoconsole to obtain a real EOF.
///
/// The close runs off-thread while the output reader remains live, as required
/// by older Windows versions. Quiet is only permission to begin that close;
/// it is never permission to report success with a reader still able to emit.
const CONPTY_DRAIN_GRACE: Duration = Duration::from_millis(750);
/// A command must never report the child's success while its PTY reader could
/// still deliver bytes. Bound the wait so a leaked Unix slave descriptor or a
/// stuck ConPTY close becomes an explicit internal failure rather than either
/// an infinite hang or successful completion with truncated output.
const PTY_EOF_TIMEOUT: Duration = Duration::from_secs(5);
/// Semantic events emitted by the VT parser. A full queue is a fail-command
/// condition because silently dropping a reply request can deadlock the child.
const PTY_EVENT_QUEUE_DEPTH: usize = 1024;
/// Bound each owner-loop output slice so a producer that continuously refills
/// the bounded handoff cannot starve timeout, cancellation, or child reaping.
const OUTPUT_SLICE_MESSAGES: usize = 16;
const OUTPUT_SLICE_BYTES: usize = 1024 * 1024;
/// Rendered stdout commands waiting behind the writer currently in the OS.
/// One additional command may remain on the lifecycle thread, keeping memory
/// bounded while leaving timeout/cancellation checks runnable.
const OUTPUT_WRITER_QUEUE_DEPTH: usize = 4;
/// Apply the same lifecycle fairness to semantic events. The queue remains
/// substantially deeper so a short burst can be absorbed without loss.
const EVENT_SLICE_MESSAGES: usize = 256;
/// Healthy local recording sinks finish well inside this bound. Crossing it is
/// treated as an observable persistence failure instead of extending command
/// completion indefinitely.
const RECORD_FINISH_TIMEOUT: Duration = Duration::from_secs(2);

/// Exit code whenever the complete `--timeout` deadline expires (coreutils
/// `timeout(1)` convention). A collected child status does not override an
/// incomplete lossless-output contract.
pub const EXIT_TIMEOUT: i32 = 124;
/// Exit code when stdout cannot accept the complete child stream (`EX_IOERR`).
pub const EXIT_OUTPUT_DELIVERY: i32 = 74;
/// Exit code for an internal kettle error (spawn failure, no PTY, bad args).
pub const EXIT_INTERNAL: i32 = 125;
/// Internal exit status when an MCP request cancels a running headless child.
pub const EXIT_CANCELLED: i32 = 130;

#[derive(Clone, Debug, Eq, PartialEq)]
struct OutputDeliveryError(String);

impl OutputDeliveryError {
    fn unexpected(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl From<std::io::Error> for OutputDeliveryError {
    fn from(error: std::io::Error) -> Self {
        Self(error.to_string())
    }
}

impl std::fmt::Display for OutputDeliveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

type OutputResult<T> = Result<T, OutputDeliveryError>;

#[derive(Clone, Copy)]
enum LifecycleStop {
    Cancellation,
    Deadline,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PtyTransportEnd {
    Pending,
    Eof,
    Failed,
    EofTimeout,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PtyDrainFailure {
    Read,
    EofTimeout,
    #[cfg(windows)]
    Containment,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PtyDrainDecision {
    Wait,
    CloseTransport,
    Finished,
    Failed(PtyDrainFailure),
}

fn observed_pty_transport_end(
    channel_disconnected: bool,
    reader_status: PtyReadStatus,
) -> PtyTransportEnd {
    match reader_status {
        // The failure is sticky, but the pump publishes it before its terminal
        // marker can pass chunks already admitted to the parser. Wait for the
        // raw sender to disconnect so those chunks can reach stdout first; the
        // final decision remains failure and can never become success.
        PtyReadStatus::Failed => {
            if channel_disconnected {
                PtyTransportEnd::Failed
            } else {
                PtyTransportEnd::Pending
            }
        }
        PtyReadStatus::EofTimeout => {
            if channel_disconnected {
                PtyTransportEnd::EofTimeout
            } else {
                PtyTransportEnd::Pending
            }
        }
        PtyReadStatus::Eof => {
            if channel_disconnected {
                PtyTransportEnd::Eof
            } else {
                PtyTransportEnd::Pending
            }
        }
        // A sender disappearing without first publishing an orderly EOF is a
        // reader failure too. This catches panic/thread-exit paths in addition
        // to explicit read errors.
        PtyReadStatus::Reading => {
            if channel_disconnected {
                PtyTransportEnd::Failed
            } else {
                PtyTransportEnd::Pending
            }
        }
    }
}

fn latest_pty_activity(
    child_gone_at: Instant,
    delivered_at: Instant,
    source_read_at: Instant,
) -> Instant {
    std::cmp::max(child_gone_at, std::cmp::max(delivered_at, source_read_at))
}

/// Decide whether a child whose process status is already gone has finished
/// producing observable PTY output.
///
/// The platform distinction is intentional. ConPTY can retain its reader
/// handle after the child and final repaint are gone, so Windows needs a
/// bounded quiet fallback. A Unix reader owns the sole raw-output sender and
/// drops it after `read` reaches EOF; elapsed time can bound that wait but can
/// never turn it into success. `transport_idle` keeps that hard bound from
/// overriding bytes already queued for delivery or a busy stdout worker.
fn pty_drain_decision(
    conpty_may_outlive_child: bool,
    transport_end: PtyTransportEnd,
    child_gone_for: Duration,
    transport_quiet_for: Duration,
    transport_idle: bool,
    source_pending_chunks: usize,
) -> PtyDrainDecision {
    match transport_end {
        PtyTransportEnd::Eof => {
            return if !transport_idle {
                // The parser may have published a chunk to the stdout worker
                // before dying without retiring its source-side count. The
                // count can no longer reach zero, but the admitted write is
                // still real work and must drain before the read failure wins.
                PtyDrainDecision::Wait
            } else if source_pending_chunks == 0 {
                PtyDrainDecision::Finished
            } else {
                // The raw sender is gone, so no reader remains that can retire
                // this count. Once downstream output is idle, waiting would
                // hang forever after a reader panic.
                PtyDrainDecision::Failed(PtyDrainFailure::Read)
            };
        }
        PtyTransportEnd::Failed => {
            return if !transport_idle {
                // Failure is sticky, but already-admitted stdout must finish
                // before the lifecycle reports it and exits the process.
                PtyDrainDecision::Wait
            } else {
                // Channel disconnection proves the sole parser is gone. Any
                // remaining source count is irreducible, so fail as soon as
                // downstream delivery is idle rather than waiting forever.
                PtyDrainDecision::Failed(PtyDrainFailure::Read)
            };
        }
        PtyTransportEnd::EofTimeout => {
            return if !transport_idle {
                PtyDrainDecision::Wait
            } else {
                PtyDrainDecision::Failed(PtyDrainFailure::EofTimeout)
            };
        }
        PtyTransportEnd::Pending => {}
    }
    if conpty_may_outlive_child {
        if transport_idle
            && source_pending_chunks == 0
            && transport_quiet_for >= SETTLE + CONPTY_DRAIN_GRACE
        {
            PtyDrainDecision::CloseTransport
        } else {
            PtyDrainDecision::Wait
        }
    } else if transport_idle && source_pending_chunks == 0 && child_gone_for >= PTY_EOF_TIMEOUT {
        PtyDrainDecision::Failed(PtyDrainFailure::EofTimeout)
    } else {
        PtyDrainDecision::Wait
    }
}

/// Production platform selection for PTY completion.
///
/// Keeping `cfg!(windows)` inside this seam lets native tests pin the actual
/// selection instead of testing only a helper with hand-written booleans.
fn native_pty_drain_decision(
    transport_end: PtyTransportEnd,
    child_gone_for: Duration,
    transport_quiet_for: Duration,
    transport_idle: bool,
    source_pending_chunks: usize,
) -> PtyDrainDecision {
    pty_drain_decision(
        cfg!(windows),
        transport_end,
        child_gone_for,
        transport_quiet_for,
        transport_idle,
        source_pending_chunks,
    )
}

fn lifecycle_stop_code(stop: LifecycleStop, _child_exit_code: Option<i32>) -> i32 {
    match stop {
        LifecycleStop::Cancellation => EXIT_CANCELLED,
        // The deadline covers the complete operation, including lossless PTY
        // delivery. A known child status cannot turn an incomplete stream into
        // success once that deadline has expired.
        LifecycleStop::Deadline => EXIT_TIMEOUT,
    }
}

/// How the child's output is rendered to stdout.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputMode {
    /// Verbatim PTY bytes (default) — exactly what the child wrote.
    Raw,
    /// ANSI escape sequences stripped — plain text for log assertions.
    StripAnsi,
    /// One JSON object per line: start / output / title / exit events.
    Json,
}

/// Parsed `kettle exec` options.
#[derive(Debug, Clone)]
pub struct ExecOpts {
    pub argv: Vec<String>,
    pub cols: u16,
    pub rows: u16,
    pub cwd: Option<PathBuf>,
    pub timeout: Option<Duration>,
    pub mode: OutputMode,
    /// Asciicast (.cast) record path — verbatim output + resize audit trail.
    pub record: Option<PathBuf>,
    /// Forward this process's stdin to the PTY (set for pipe/file/socket stdin
    /// — see `stdin_is_pipe`).
    pub forward_stdin: bool,
}

/// Maximum unterminated OSC/DCS/APC/PM/SOS payload discarded before the
/// stripper resynchronizes. The parser stores only finite state, but a bound is
/// still required so a malicious child cannot suppress all later plaintext.
const MAX_CONTROL_SEQUENCE_BYTES: usize = 64 * 1024;

#[derive(Debug, Default)]
pub struct AnsiStripper {
    state: StripState,
    /// UTF-8 continuation bytes still owed by the character being decoded.
    ///
    /// Terminal output is UTF-8, and the C1 control bytes this stripper
    /// recognizes — `0x90` DCS, `0x98` SOS, `0x9b` CSI, `0x9d` OSC, `0x9e` PM,
    /// `0x9f` APC — are all in the `0x80..=0xbf` continuation range. Treating
    /// them as controls wherever they appeared ate the middle of ordinary
    /// characters and left invalid UTF-8 behind:
    ///
    /// * `Û` is `c3 9b` — the `9b` was read as CSI, so `Ûh` became a lone `c3`.
    /// * `‘` is `e2 80 98` — the `98` was read as SOS.
    /// * `▐` is `e2 96 90` — the `90` was read as DCS.
    ///
    /// Box-drawing and smart quotes are exactly what a TUI emits, and MCP's
    /// `kettle_run` strips by default, so this corrupted the output an agent
    /// reads. Counting the sequence keeps a continuation byte a continuation
    /// byte; a C1 byte in ground position, where UTF-8 could not put one, is
    /// still honored for genuinely 8-bit streams.
    utf8_continuation: u8,
    /// Whether the lead byte of that character reached `out`.
    ///
    /// A character is emitted whole or swallowed whole; the state machine may
    /// move on between its bytes and must not get a vote. See the shield in
    /// [`AnsiStripper::push`].
    utf8_emitted: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum StripState {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate {
        remaining: usize,
    },
    Csi {
        remaining: usize,
    },
    String {
        bel_terminated: bool,
        escaped: bool,
        remaining: usize,
    },
}

impl AnsiStripper {
    /// Feed a chunk; append the stripped plaintext to `out`.
    pub fn push(&mut self, input: &[u8], out: &mut Vec<u8>) {
        for &b in input {
            // Mid-character: a continuation byte belongs to the character being
            // decoded and can never be a control, in ANY state. Tracking this
            // only in Ground still let `0x9c` inside an OSC read as ST — so
            // `ESC ] 0 ; ✳ title BEL` (`✳` is `e2 9c b3`) terminated the string
            // early and leaked the rest of it as visible text.
            if self.utf8_continuation > 0 {
                if matches!(b, 0x80..=0xbf) {
                    self.utf8_continuation -= 1;
                    // A continuation goes wherever its LEAD went, not wherever
                    // the state machine has since arrived. Asking the current
                    // state instead split characters in half: the forced
                    // resynchronization that ends an over-long control string
                    // can land on a lead byte, swallowing it and leaving the
                    // machine in ground — and the continuations were then
                    // emitted with no lead in front of them, so everything
                    // decoding stdout saw invalid UTF-8 from that point on.
                    if self.utf8_emitted {
                        out.push(b);
                    }
                    continue;
                }
                // Not a continuation after all: the lead was malformed or
                // truncated. Stop shielding immediately and let this byte be
                // interpreted normally — blindly swallowing N bytes hid real
                // C1 controls and desynchronized on a lead-followed-by-lead.
                self.utf8_continuation = 0;
            }
            // A lead byte is tracked in EVERY state, not only where text is
            // read.
            //
            // Restricting this to `Ground | String` left the same split-character
            // hole in the other two bounded states. `ESC [` followed by 64 KiB of
            // parameter bytes forces a resynchronization out of `Csi`, and if
            // that bound lands on a lead byte the lead is consumed there while
            // its continuations arrive in ground — emitted with nothing in front
            // of them. `EscapeIntermediate` has the identical bound, and `ESC`
            // followed directly by a lead byte reaches it with no 64 KiB
            // required at all.
            //
            // Tracking everywhere is also strictly safer than not: a byte in
            // `0xc2..=0xf4` is not a legal CSI parameter, intermediate, or final
            // byte, so the only streams this changes are already malformed — and
            // if the bytes that follow turn out NOT to be continuations, the
            // shield above releases them on the spot.
            self.utf8_continuation = match b {
                0xc2..=0xdf => 1,
                0xe0..=0xef => 2,
                0xf0..=0xf4 => 3,
                // ASCII, a stray continuation, or an invalid lead
                // (0xc0/0xc1/0xf5..) owes nothing and is interpreted as
                // itself.
                _ => 0,
            };
            // No lead byte is special to the ground arm below, so a lead reaches
            // `out` exactly when the machine is in ground — and its
            // continuations must go wherever it went.
            self.utf8_emitted = matches!(self.state, StripState::Ground);
            let state = std::mem::take(&mut self.state);
            self.state = match state {
                StripState::Ground => match b {
                    0x1b => StripState::Escape,
                    0x9b => Self::csi(),
                    0x9d => Self::string(true),
                    0x90 | 0x98 | 0x9e | 0x9f => Self::string(false),
                    0x9c => StripState::Ground,
                    _ => {
                        out.push(b);
                        StripState::Ground
                    }
                },
                StripState::Escape => Self::after_escape(b),
                StripState::EscapeIntermediate { remaining } => {
                    if matches!(b, 0x18 | 0x1a) {
                        StripState::Ground
                    } else if b == 0x1b {
                        StripState::Escape
                    } else if (0x30..=0x7e).contains(&b) || remaining <= 1 {
                        StripState::Ground
                    } else {
                        StripState::EscapeIntermediate {
                            remaining: remaining - 1,
                        }
                    }
                }
                StripState::Csi { remaining } => {
                    if matches!(b, 0x18 | 0x1a) {
                        StripState::Ground
                    } else if b == 0x1b {
                        StripState::Escape
                    } else if (0x40..=0x7e).contains(&b) || remaining <= 1 {
                        StripState::Ground
                    } else {
                        StripState::Csi {
                            remaining: remaining - 1,
                        }
                    }
                }
                StripState::String {
                    bel_terminated,
                    escaped,
                    remaining,
                } => {
                    if matches!(b, 0x18 | 0x1a)
                        || b == 0x9c
                        || (bel_terminated && b == 0x07)
                        || (escaped && b == b'\\')
                    {
                        StripState::Ground
                    } else if escaped {
                        // ESC starts a fresh escape sequence from every control
                        // string. OSC used to be the lone exception because
                        // its BEL terminator was folded into this condition:
                        // `ESC ] title ESC c` therefore swallowed all later
                        // text while DCS/APC/PM/SOS correctly dispatched `c`.
                        // The terminators above still win for ESC \\ and BEL.
                        Self::after_escape(b)
                    } else if remaining <= 1 {
                        // Forced resynchronization: an unterminated control
                        // string cannot hold the stream hostage, so it ends
                        // here. This byte was swallowed as part of the string —
                        // and if it was a UTF-8 lead, `utf8_emitted` is already
                        // false, so its continuations are swallowed with it
                        // rather than surfacing in ground output alone.
                        StripState::Ground
                    } else {
                        StripState::String {
                            bel_terminated,
                            escaped: b == 0x1b,
                            remaining: remaining - 1,
                        }
                    }
                }
            };
        }
    }

    fn csi() -> StripState {
        StripState::Csi {
            remaining: MAX_CONTROL_SEQUENCE_BYTES,
        }
    }

    fn after_escape(b: u8) -> StripState {
        match b {
            b'[' => Self::csi(),
            b']' => Self::string(true),
            b'P' | b'X' | b'^' | b'_' => Self::string(false),
            0x20..=0x2f => StripState::EscapeIntermediate {
                remaining: MAX_CONTROL_SEQUENCE_BYTES,
            },
            0x1b => StripState::Escape,
            _ => StripState::Ground,
        }
    }

    fn string(bel_terminated: bool) -> StripState {
        StripState::String {
            bel_terminated,
            escaped: false,
            remaining: MAX_CONTROL_SEQUENCE_BYTES,
        }
    }
}

/// A private handle to this process's standard output for the child stream.
///
/// `Outputter` flushes after every command, so the process-global
/// `std::io::stdout()` contributes nothing but a shared lock and a second copy
/// of every byte on the hottest path Kettle has. It also costs correctness on
/// Unix: bytes a failed write leaves in that global buffer are retried by the
/// runtime's exit-time flush, on the main thread, where SIGPIPE is back at
/// SIG_DFL — so a broken pipe kills the process by signal and discards the
/// lifecycle's chosen exit code. A duplicated descriptor keeps exec's output
/// out of that buffer entirely.
///
/// Windows keeps the standard handle deliberately: it has no SIGPIPE, and
/// `Stdout` there transcodes to UTF-16 for `WriteConsoleW`. Raw handle writes
/// would emit UTF-8 bytes for the console's active code page to misread
/// whenever stdout is a console rather than a pipe.
#[cfg(unix)]
fn exec_stdout_sink() -> std::io::Result<std::fs::File> {
    use std::os::fd::AsFd as _;
    Ok(std::fs::File::from(
        std::io::stdout().as_fd().try_clone_to_owned()?,
    ))
}

#[cfg(not(unix))]
fn exec_stdout_sink() -> std::io::Result<std::io::Stdout> {
    Ok(std::io::stdout())
}

/// Run `kettle exec` end to end; returns the process exit code to propagate.
pub fn run_exec(opts: ExecOpts) -> i32 {
    let sink = match exec_stdout_sink() {
        Ok(sink) => sink,
        Err(error) => {
            let _ = writeln!(
                std::io::stderr(),
                "kettle exec: cannot open stdout for the child stream: {error}"
            );
            return EXIT_INTERNAL;
        }
    };
    let mut output = match WorkerOutput::spawn(opts.mode, sink) {
        Ok(output) => output,
        Err(error) => {
            let _ = writeln!(
                std::io::stderr(),
                "kettle exec: cannot start stdout writer: {error}"
            );
            return EXIT_INTERNAL;
        }
    };
    run_exec_engine(opts, &default_size_probe, &mut output, None, false)
}

/// (agent-first A3): run a command headlessly and CAPTURE its output
/// in-process (instead of streaming to stdout) — the engine behind the
/// `kettle_run` MCP tool. Returns `(exit_code, output)`; the output is the
/// tail-capped (1 MiB) child output in the requested mode (strip-ansi
/// recommended for agent assertions).
pub fn run_exec_capture(opts: ExecOpts) -> (i32, String) {
    run_exec_capture_inner(opts, None)
}

/// Capture a headless command while observing an MCP cancellation flag. When
/// cancellation is requested, the child is killed, output is settle-drained,
/// and any recorder is finalized before returning [`EXIT_CANCELLED`].
pub fn run_exec_capture_cancellable(opts: ExecOpts, cancelled: &AtomicBool) -> (i32, String) {
    run_exec_capture_inner(opts, Some(cancelled))
}

/// A sink that keeps only the last `cap` bytes, so an unbounded producer cannot
/// exhaust memory. Agents want "what just happened" anyway.
///
/// Trimming to exactly `cap` on every write made this quadratic in the output
/// volume: once full, a 4-KiB chunk shifted the whole 1-MiB buffer down by 4
/// KiB. A build emitting 100 MiB moved ~25 GiB of memory to keep the last 1 MiB
/// of it, on the thread draining the PTY. Letting the buffer run to `cap` bytes
/// of slack before compacting makes each shift pay for at least `cap` bytes of
/// input, so the total work is linear. The peak cost is one extra `cap` of
/// memory.
struct TailSink {
    buf: Vec<u8>,
    cap: usize,
}

impl TailSink {
    fn new(cap: usize) -> Self {
        Self {
            buf: Vec::new(),
            cap,
        }
    }

    /// The retained tail, at most `cap` bytes.
    ///
    /// Compaction is lazy, so the buffer can hold more than that between
    /// writes; the excess is always at the FRONT, which is the part being
    /// dropped.
    fn tail(&self) -> &[u8] {
        &self.buf[self.buf.len().saturating_sub(self.cap)..]
    }
}

impl Write for TailSink {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        // A single write at least as large as the cap discards everything held
        // so far, so copy only the part that survives rather than growing the
        // buffer to the size of the write.
        if data.len() >= self.cap {
            self.buf.clear();
            self.buf.extend_from_slice(&data[data.len() - self.cap..]);
            return Ok(data.len());
        }
        self.buf.extend_from_slice(data);
        // Compact only once the slack is used up. `saturating_mul` keeps an
        // enormous cap from wrapping the threshold to something small.
        if self.buf.len() > self.cap.saturating_mul(2) {
            let drop = self.buf.len() - self.cap;
            self.buf.drain(..drop);
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

fn run_exec_capture_inner(opts: ExecOpts, cancelled: Option<&AtomicBool>) -> (i32, String) {
    let mut sink = TailSink::new(1024 * 1024);
    let mut output = DirectOutput::new(opts.mode, &mut sink);
    let code = run_exec_engine(opts, &default_size_probe, &mut output, cancelled, true);
    (code, String::from_utf8_lossy(sink.tail()).into_owned())
}

/// Default console-size probe (real terminal dimensions when stdout is a TTY).
pub fn default_size_probe() -> Option<(u16, u16)> {
    terminal_size_cols_rows()
}

/// The production source of this file, excluding test-only items.
#[cfg(test)]
fn production_source() -> String {
    let production = kettle_test_support::production_source(include_str!("exec.rs"));
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

/// Core run loop, with the stdout sink and size probe injected for testing.
#[cfg(test)]
pub fn run_exec_with(
    opts: ExecOpts,
    _size_probe: &dyn Fn() -> Option<(u16, u16)>,
    sink: &mut dyn Write,
) -> i32 {
    let mut output = DirectOutput::new(opts.mode, sink);
    run_exec_engine(opts, _size_probe, &mut output, None, false)
}

/// Drain one bounded output slice and report whether a backlog remains.
///
/// A bounded channel alone does not make an unbounded `try_recv` loop fair:
/// the producer can refill each slot as soon as the owner removes it. Both a
/// message and byte budget keep lifecycle checks reachable under that race.
/// `pty_channel_disconnected` is latched when the raw channel reports
/// *disconnected*
/// rather than merely empty.
///
/// That distinction is the whole point. An empty channel is not evidence the
/// PTY has been read to the end — it is equally consistent with the reader
/// thread not having been scheduled yet, which is routine on a loaded machine.
/// A disconnected one IS evidence: the reader owns the only sender and drops it
/// on the way out of its loop, after EOF.
///
/// Treating "empty" as "finished" cost the child's output. For a command that
/// writes a little and exits at once, the exit status could be observed and the
/// settle window elapse while the bytes were still in flight; the loop then
/// finished the recorder and closed stdout, and they arrived with nowhere to
/// go. It showed up as two macOS intermittents that looked unrelated —
/// `exec_streams_stdout_and_exits_zero` returning exit 0 with empty stdout, and
/// `exec_record_writes_replayable_asciicast` writing a trace containing only its
/// header — because one gate feeds both stdout and the recorder.
fn drain_output_slice(
    receiver: &Receiver<Vec<u8>>,
    recorder: &mut Option<kettle_core::record::Recorder>,
    output: &mut dyn ExecOutput,
    pty_channel_disconnected: &std::cell::Cell<bool>,
    last_pty_output_at: &std::cell::Cell<Instant>,
) -> OutputResult<bool> {
    let mut bytes_drained = 0usize;
    for _ in 0..OUTPUT_SLICE_MESSAGES {
        if bytes_drained >= OUTPUT_SLICE_BYTES {
            break;
        }
        if !output.ready()? {
            return Ok(true);
        }
        let bytes = match receiver.try_recv() {
            Ok(bytes) => bytes,
            Err(crossbeam_channel::TryRecvError::Empty) => return Ok(false),
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                pty_channel_disconnected.set(true);
                return Ok(false);
            }
        };
        last_pty_output_at.set(Instant::now());
        bytes_drained = bytes_drained.saturating_add(bytes.len());
        record_chunk(recorder, &bytes);
        output.output(bytes)?;
    }
    Ok(!receiver.is_empty() || !output.ready()?)
}

/// Preserve the audit trace during bounded teardown after stdout has stopped
/// accepting commands. These raw chunks are intentionally not republished:
/// timeout/cancellation semantics abandon output the consumer has not accepted
/// rather than letting that consumer delay child reaping.
fn drain_recording_slice(
    receiver: &Receiver<Vec<u8>>,
    recorder: &mut Option<kettle_core::record::Recorder>,
) {
    let mut bytes_drained = 0usize;
    for _ in 0..OUTPUT_SLICE_MESSAGES {
        if bytes_drained >= OUTPUT_SLICE_BYTES {
            break;
        }
        let Ok(bytes) = receiver.try_recv() else {
            break;
        };
        bytes_drained = bytes_drained.saturating_add(bytes.len());
        record_chunk(recorder, &bytes);
    }
}

/// Make the chosen exit code final once stdout has failed.
///
/// `main` restores SIGPIPE to SIG_DFL so ordinary pipelines
/// (`kettle --list-themes | head`) die quietly like every other CLI. That
/// convention is wrong from here on: the lifecycle has already converted the
/// broken pipe into exit 74 and explained it on stderr. A later EPIPE — the
/// runtime flushing whatever another code path had buffered on the global
/// stdout, on the main thread, during `process::exit` — would kill Kettle by
/// signal and discard both the code and the diagnostic.
#[cfg(unix)]
fn commit_output_failure_exit() {
    // SAFETY: `signal` is async-signal-safe and SIG_IGN installs no handler
    // state. The exec lifecycle has already stopped writing to stdout, so no
    // thread can be mid-write when the disposition changes.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

#[cfg(not(unix))]
fn commit_output_failure_exit() {}

fn stop_after_output_failure(
    error: OutputDeliveryError,
    term: &Terminal,
    process_tree: &ExecProcessTree,
    raw_output: &Receiver<Vec<u8>>,
    recorder: &mut Option<kettle_core::record::Recorder>,
    output: &mut dyn ExecOutput,
    started: Instant,
) -> i32 {
    commit_output_failure_exit();
    let _ = writeln!(
        std::io::stderr(),
        "kettle exec: stdout delivery failed: {error}"
    );
    let contained = process_tree.terminate(term);
    let _ = wait_for_exit_code(term);
    std::thread::sleep(SETTLE);
    drain_recording_slice(raw_output, recorder);
    finish_recording(recorder, Duration::ZERO);
    let code = if contained {
        EXIT_OUTPUT_DELIVERY
    } else {
        let _ = writeln!(
            std::io::stderr(),
            "kettle exec: process-tree teardown was not verified; reporting internal failure"
        );
        EXIT_INTERNAL
    };
    let _ = output.finish(code, started.elapsed(), OutputFinish::AbandonPending);
    code
}

/// Drain one bounded semantic-event slice and report whether work remains.
#[cfg(test)]
fn drain_event_slice<T>(receiver: &Receiver<T>, mut handle: impl FnMut(T)) -> bool {
    drain_event_slice_until(receiver, |event| {
        handle(event);
        true
    })
}

/// Drain semantic events until the budget is spent or `handle` asks to stop.
/// The latter lets an ordered JSON title wait behind a saturated stdout writer
/// without consuming later title events or blocking lifecycle checks.
fn drain_event_slice_until<T>(receiver: &Receiver<T>, mut handle: impl FnMut(T) -> bool) -> bool {
    for _ in 0..EVENT_SLICE_MESSAGES {
        let Ok(event) = receiver.try_recv() else {
            return false;
        };
        if !handle(event) {
            return true;
        }
    }
    !receiver.is_empty()
}

/// Report a failure that happens before the PTY exists.
///
/// The message still goes to stderr, and now also into the output sink when
/// that sink is a plain byte stream. `run_exec_capture` returns only an exit
/// code and that sink, and the MCP `kettle_run` tool builds its whole reply
/// from those two, so an agent that typo'd a binary name received
/// `exit code: 125` and nothing else. Command-not-found and a bad working
/// directory are the two most common ways that tool fails, and 125 is
/// documented as an internal error, so the agent concluded kettle was broken
/// rather than fixing its own argument.
///
/// JSON mode is deliberately left alone. Its stream is a documented sequence
/// that opens with a `start` event, and emitting an `output` event before one
/// would be a worse answer than the missing text.
fn startup_failure(
    mode: OutputMode,
    capturing: bool,
    output: &mut dyn ExecOutput,
    message: String,
) -> i32 {
    let _ = writeln!(std::io::stderr(), "{message}");
    // Only for a capturing caller. Writing it to a terminal's stdout as well
    // would print the same line twice to someone who can already see it.
    if capturing && matches!(mode, OutputMode::Raw | OutputMode::StripAnsi) {
        let _ = output.output(format!("{message}\n").into_bytes());
    }
    EXIT_INTERNAL
}

fn run_exec_engine(
    mut opts: ExecOpts,
    _size_probe: &dyn Fn() -> Option<(u16, u16)>,
    output: &mut dyn ExecOutput,
    cancelled: Option<&AtomicBool>,
    capturing: bool,
) -> i32 {
    if opts.argv.is_empty() {
        return startup_failure(
            opts.mode,
            capturing,
            output,
            "kettle exec: no command given".to_string(),
        );
    }
    // Clamp geometry into a sane PTY range (kettle-core clamps too, but a 0
    // here would make ConPTY unhappy).
    opts.cols = opts.cols.clamp(1, u16::MAX);
    opts.rows = opts.rows.clamp(1, u16::MAX);

    let cwd = match validate_exec_cwd(opts.cwd.as_deref()) {
        Ok(cwd) => cwd,
        Err(error) => {
            return startup_failure(
                opts.mode,
                capturing,
                output,
                format!("kettle exec: invalid --cwd: {error}"),
            );
        }
    };

    let (tx, rx): (Sender<TermEvent>, Receiver<TermEvent>) =
        crossbeam_channel::bounded(PTY_EVENT_QUEUE_DEPTH);
    let (otx, orx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = crossbeam_channel::bounded(4);
    // Latched once the raw channel reports disconnected rather than empty --
    // see `drain_output_slice`. A `Cell` because the lifecycle loop is single
    // threaded and this is only ever set from inside it.
    let pty_channel_disconnected = std::cell::Cell::new(false);
    let (stdin_tx, stdin_rx) = crossbeam_channel::bounded::<StdinPumpEvent>(4);
    let (pty_reply_tx, pty_reply_rx) = crossbeam_channel::bounded::<Vec<u8>>(64);
    let pty_reply_gate = Arc::new(Mutex::new(()));
    let (stdin_done_tx, stdin_done_rx) = crossbeam_channel::unbounded::<StdinPumpResult>();
    let waker: Waker = std::sync::Arc::new(|| {});

    // Recording is an explicit audit request for `kettle exec`. Establish it
    // before the PTY exists and fail closed if the path cannot be secured; a
    // child must never run after Kettle has silently lost the requested trace.
    let mut recorder = match opts.record.as_ref() {
        Some(path) => match kettle_core::record::Recorder::start(path, opts.cols, opts.rows, false)
        {
            Ok(recorder) => Some(recorder),
            Err(error) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "kettle exec: cannot start --record trace: {error}"
                );
                return EXIT_INTERNAL;
            }
        },
        None => None,
    };

    #[allow(unused_mut)] // mutated only by the Windows ConPTY close handshake
    let mut term = match Terminal::new_with_env_and_output_geometry_capabilities_and_cwd_policy(
        &opts.argv,
        cwd,
        // Modest scrollback — exec output streams out immediately, the grid is
        // only used for VT state + query answers.
        2000,
        0,
        PtyGeometry::from_cell_size(opts.cols as usize, opts.rows as usize, 8, 16),
        false,
        CursorShape::Block,
        None,
        "xterm-256color",
        "truecolor",
        &[],
        false,
        // No shell-integration injection — `kettle exec` runs a one-shot
        // non-interactive command, not an interactive shell.
        false,
        // Headless exec has no clipboard sink. Do not advertise DA1 extension
        // 52 when OSC 52 writes would be deliberately ignored.
        TerminalCapabilities {
            osc52_copy: false,
            contain_process_tree: true,
            ..TerminalCapabilities::default()
        },
        // An explicit automation cwd is a contract. Passing it to the OS even
        // after the preflight closes the deletion race: spawn fails instead of
        // silently falling back to HOME if the directory vanishes.
        WorkingDirectoryPolicy::RejectInvalidExplicit,
        tx,
        waker,
        Some(PtyOutputSender::lossless(otx)),
    ) {
        Ok(t) => t,
        Err(e) => {
            return startup_failure(
                opts.mode,
                capturing,
                output,
                format!("kettle exec: cannot start PTY: {e}"),
            );
        }
    };
    let process_tree = ExecProcessTree::attach(&term);

    // Every PTY write goes through the bounded arbiter, even when this process
    // is not forwarding stdin. A child can flood terminal queries without ever
    // reading their replies; writing those replies on the lifecycle thread
    // would let a full PTY input queue defeat timeout and cancellation.
    let stdin_handle = match term.stdin_handle() {
        Ok(handle) => handle,
        Err(error) => {
            let _ = writeln!(
                std::io::stderr(),
                "kettle exec: cannot prepare PTY writer arbitration: {error}"
            );
            return EXIT_INTERNAL;
        }
    };
    if let Err(error) = spawn_pty_writer_arbiter(
        stdin_handle,
        pty_reply_rx,
        Arc::clone(&pty_reply_gate),
        stdin_rx,
        stdin_done_tx,
    ) {
        let _ = writeln!(
            std::io::stderr(),
            "kettle exec: cannot start PTY writer arbiter: {error}"
        );
        return EXIT_INTERNAL;
    }
    if opts.forward_stdin {
        if let Err(error) = spawn_stdin_reader(stdin_tx) {
            let _ = writeln!(
                std::io::stderr(),
                "kettle exec: cannot start stdin forwarding thread: {error}"
            );
            return EXIT_INTERNAL;
        }
    } else {
        drop(stdin_tx);
    }

    let started = Instant::now();
    let last_pty_output_at = std::cell::Cell::new(started);
    let initial_read_progress = term.pty_read_progress();
    let mut observed_read_generation = initial_read_progress.generation;
    let last_pty_source_activity_at = std::cell::Cell::new(started);
    macro_rules! output_or_stop {
        ($operation:expr) => {
            match $operation {
                Ok(value) => value,
                Err(error) => {
                    return stop_after_output_failure(
                        error,
                        &term,
                        &process_tree,
                        &orx,
                        &mut recorder,
                        output,
                        started,
                    );
                }
            }
        };
    }
    output_or_stop!(output.start(opts.cols, opts.rows));

    let mut child_gone_at: Option<Instant> = None;
    let mut child_exit_code: Option<i32> = None;
    let mut completion_code: Option<i32> = None;
    let mut recording_finish_deadline: Option<Instant> = None;
    #[cfg(windows)]
    let mut pty_close_started_at: Option<Instant> = None;
    let mut last_lifecycle_trace = started;

    loop {
        // Evaluate both externally imposed stop conditions before touching any
        // tracing, output, event, stdin, or child-status work. Those operations
        // are intended to be bounded, but a regression in one of them must
        // never park the deadline/cancellation decision behind downstream
        // pressure.
        let elapsed = started.elapsed();
        let cancellation_requested = cancelled.is_some_and(|flag| flag.load(Ordering::Acquire));
        let timeout_expired = opts.timeout.is_some_and(|limit| elapsed >= limit);
        // Do not reap the direct child merely because a stop became due. On
        // Linux its unreaped identity is the kernel-held anchor proving that
        // the numeric PTY session and process-group ids have not been recycled.
        // The status is collected only after ordinary PTY/output completion;
        // timeout and cancellation have their own fixed exit codes.
        let lifecycle_stop = if cancellation_requested {
            Some(LifecycleStop::Cancellation)
        } else if timeout_expired {
            Some(LifecycleStop::Deadline)
        } else {
            None
        };
        if let Some(stop) = lifecycle_stop {
            let reason = match stop {
                LifecycleStop::Cancellation => "cancellation",
                LifecycleStop::Deadline => "timeout",
            };
            let requested_code = lifecycle_stop_code(stop, child_exit_code);
            log::debug!(
                "kettle exec {reason} reached; starting bounded teardown with exit code \
                 {requested_code}"
            );
            let contained = process_tree.terminate(&term);
            let code = if contained {
                requested_code
            } else {
                let _ = writeln!(
                    std::io::stderr(),
                    "kettle exec: {reason} process-tree teardown was not verified; \
                     reporting internal failure instead of completed {reason}"
                );
                EXIT_INTERNAL
            };
            if matches!(stop, LifecycleStop::Cancellation) {
                // Reap the killed child where the PTY backend exposes its
                // status; this prevents a cancelled long-running MCP tool from
                // leaving a zombie behind while still bounding cancellation
                // latency.
                let _ = wait_for_exit_code(&term);
            }
            std::thread::sleep(SETTLE);
            // Once an imposed stop wins, never consult downstream output
            // readiness again. Preserve the bounded audit tail directly from
            // the raw PTY channel, then apply the explicit abandon contract.
            drain_recording_slice(&orx, &mut recorder);
            finish_recording(&mut recorder, Duration::ZERO);
            let _ = output.finish(code, started.elapsed(), OutputFinish::AbandonPending);
            log::debug!("kettle exec bounded stop teardown finished");
            return code;
        }

        if let Some(recorder) = recorder.as_mut() {
            report_recording_status(recorder);
        }

        // Ordinary completion is also driven asynchronously. In particular,
        // never join the stdout worker on this lifecycle thread: its final
        // write/flush can become blocked after the child has exited.
        if let Some(code) = completion_code {
            let _ = output_or_stop!(output.ready());
            let output_complete = output_or_stop!(output.completion_ready());
            let recording_complete = poll_recording_finish(
                &mut recorder,
                recording_finish_deadline
                    .expect("ordinary completion must bound recorder finalization"),
            );
            if output_complete && recording_complete {
                // This is the first point after status observation at which no
                // timeout/cancellation can still supersede ordinary
                // completion. Consume the Unix wait status now, releasing the
                // Linux identity anchor immediately before returning.
                let _ = term.child_exit_code();
                return code;
            }
            std::thread::sleep(Duration::from_millis(8));
            continue;
        }

        let trace_lifecycle = elapsed >= Duration::from_secs(4)
            && last_lifecycle_trace.elapsed() >= Duration::from_millis(250);
        if trace_lifecycle {
            last_lifecycle_trace = Instant::now();
            log::debug!(
                "kettle exec lifecycle turn: elapsed={elapsed:?}, cancelled={cancellation_requested}, \
                 timeout={:?}, timeout_expired={timeout_expired}, child_gone={}",
                opts.timeout,
                child_gone_at.is_some()
            );
        }

        // Drain output first, but keep the slice finite. A continuously
        // refilled queue must not hide timeout/cancellation indefinitely.
        let output_backlog = output_or_stop!(drain_output_slice(
            &orx,
            &mut recorder,
            output,
            &pty_channel_disconnected,
            &last_pty_output_at
        ));
        if trace_lifecycle {
            log::debug!("kettle exec lifecycle output slice returned: backlog={output_backlog}");
        }
        // Service the child's terminal queries + lifecycle events.
        let mut pty_writer_error: Option<String> = None;
        let mut queue_reply = |bytes: &[u8]| {
            let reply = bytes.to_vec();
            // Serialize reply publication with the arbiter's final EOF
            // recheck. Without this gate, the worker can observe an empty
            // channel, lose its timeslice, then inject VEOF after a reply was
            // queued. The lock protects ordering only; poisoning cannot make
            // the unit value inconsistent, so recovery is safe.
            let send_result = {
                let _gate = pty_reply_gate
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                pty_reply_tx.try_send(reply)
            };
            match send_result {
                Ok(()) => {}
                Err(crossbeam_channel::TrySendError::Full(_)) => {
                    pty_writer_error.get_or_insert_with(|| {
                        "PTY reply queue exceeded its 64-message bound".into()
                    });
                }
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                    pty_writer_error
                        .get_or_insert_with(|| "PTY writer arbiter stopped unexpectedly".into());
                }
            }
        };
        let mut output_failure = None;
        let event_output_ready = match output.ready() {
            Ok(ready) => ready,
            Err(error) => {
                output_failure = Some(error);
                false
            }
        };
        let event_backlog = if event_output_ready {
            drain_event_slice_until(&rx, |ev| {
                match ev {
                    TermEvent::PtyWrite(s) => queue_reply(s.as_bytes()),
                    TermEvent::Title(t) => {
                        if let Err(error) = output.title(t) {
                            output_failure = Some(error);
                            return false;
                        }
                    }
                    TermEvent::TextAreaSizeRequest(fmt) => {
                        let (pixel_width, pixel_height) = term.pty_pixel_size();
                        let reply = kettle_render::reply_for_text_area_size(
                            pixel_width,
                            pixel_height,
                            &*fmt,
                        );
                        queue_reply(reply.as_bytes());
                    }
                    TermEvent::ColorRequest(idx, fmt) => {
                        if let Some(s) = term.term.lock().ok().and_then(|t| {
                            kettle_render::reply_for_query(
                                idx,
                                &kettle_config::Theme::default(),
                                t.colors(),
                                &*fmt,
                            )
                        }) {
                            queue_reply(s.as_bytes());
                        }
                    }
                    // Headless exec has no clipboard sink and advertises no DA1
                    // extension 52. Discard writes explicitly at this boundary.
                    TermEvent::ClipboardStore(_, _) => {}
                    // OSC 52 read: deny (reply empty so the protocol stays
                    // well-formed without leaking a clipboard to a headless child).
                    TermEvent::ClipboardLoad(_, fmt) => queue_reply(fmt("").as_bytes()),
                    TermEvent::Exit | TermEvent::ChildExit(_) => {
                        // This event is a PTY/parser lifecycle notification,
                        // not a portable non-reaping process-status query.
                        // In particular, a leaked Unix slave prevents it from
                        // arriving at all. The explicit status observation
                        // below owns `child_gone_at` on every lifecycle turn.
                        log::debug!("kettle exec handling terminal-exit event");
                    }
                    _ => {}
                }
                match output.ready() {
                    Ok(ready) => ready,
                    Err(error) => {
                        output_failure = Some(error);
                        false
                    }
                }
            })
        } else {
            !rx.is_empty()
        };
        if let Some(error) = output_failure {
            return stop_after_output_failure(
                error,
                &term,
                &process_tree,
                &orx,
                &mut recorder,
                output,
                started,
            );
        }
        if trace_lifecycle {
            log::debug!("kettle exec lifecycle event slice returned: backlog={event_backlog}");
        }
        if term.event_queue_overflowed() {
            pty_writer_error.get_or_insert_with(|| {
                format!(
                    "PTY semantic event queue exceeded its {PTY_EVENT_QUEUE_DEPTH}-message bound"
                )
            });
        }
        let fatal_pty_error = pty_writer_error;
        let mut stdin_forwarding_error = None;
        while let Ok(result) = stdin_done_rx.try_recv() {
            match result {
                StdinPumpResult::ReadError(error) => {
                    let _ = writeln!(
                        std::io::stderr(),
                        "kettle exec: stdin forwarding ended after a read error: {error}; \
                         keeping the PTY open"
                    );
                }
                StdinPumpResult::ForwardError(error) => {
                    stdin_forwarding_error = Some(error);
                }
                StdinPumpResult::Eof(result) => match result {
                    Ok(true) => {}
                    Ok(false) => {
                        let _ = writeln!(
                            std::io::stderr(),
                            "kettle exec: stdin reached EOF but this PTY has no safe EOF \
                                 signal (noncanonical Unix input or Windows ConPTY); keeping \
                                 the PTY open for terminal replies (use an application delimiter \
                                 or --timeout)"
                        );
                    }
                    Err(error) => {
                        let _ = writeln!(
                            std::io::stderr(),
                            "kettle exec: cannot safely signal child stdin EOF: {error}; \
                                 keeping the PTY open for terminal replies"
                        );
                    }
                },
            }
        }
        if trace_lifecycle {
            log::debug!(
                "kettle exec lifecycle stdin results returned: fatal={}, forwarding={}",
                fatal_pty_error.is_some(),
                stdin_forwarding_error.is_some()
            );
        }
        if let Some(error) = fatal_pty_error {
            let _ = writeln!(
                std::io::stderr(),
                "kettle exec: cannot safely service child PTY events: {error}"
            );
            let _ = process_tree.terminate(&term);
            let _ = wait_for_exit_code(&term);
            std::thread::sleep(SETTLE);
            let _ = output_or_stop!(drain_output_slice(
                &orx,
                &mut recorder,
                output,
                &pty_channel_disconnected,
                &last_pty_output_at
            ));
            finish_recording(&mut recorder, Duration::ZERO);
            let _ = output.finish(
                EXIT_INTERNAL,
                started.elapsed(),
                OutputFinish::AbandonPending,
            );
            return EXIT_INTERNAL;
        }

        // Observe process exit independently of PTY EOF. A background process
        // can inherit the slave and keep the reader alive after the direct
        // child exits, which is exactly when Unix's bounded no-EOF clock must
        // start. On Unix `waitid(WNOWAIT)` preserves the zombie and its numeric
        // identity until stdout and recording have both acknowledged ordinary
        // completion, so a later deadline can still authenticate Linux session
        // teardown. Windows process handles have no reaping identity hazard.
        let observed_child_status: std::io::Result<Option<i32>> = if child_gone_at.is_none() {
            #[cfg(unix)]
            {
                term.child_exit_code_unreaped()
                    .map(|status| status.map(clamp_code))
            }
            #[cfg(windows)]
            {
                Ok(term.child_exit_code().map(clamp_code))
            }
            #[cfg(not(any(unix, windows)))]
            {
                Ok(term.child_exit_code().map(clamp_code))
            }
        } else {
            Ok(None)
        };
        match observed_child_status {
            Ok(Some(code)) => {
                child_gone_at = Some(Instant::now());
                child_exit_code = Some(code);
            }
            Ok(None) => {}
            Err(error) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "kettle exec: cannot observe child exit without losing teardown identity: \
                     {error}"
                );
                let _ = process_tree.terminate(&term);
                let _ = wait_for_exit_code(&term);
                std::thread::sleep(SETTLE);
                drain_recording_slice(&orx, &mut recorder);
                finish_recording(&mut recorder, Duration::ZERO);
                let _ = output.finish(
                    EXIT_INTERNAL,
                    started.elapsed(),
                    OutputFinish::AbandonPending,
                );
                return EXIT_INTERNAL;
            }
        }
        if let Some(error) = stdin_forwarding_error {
            log::debug!("kettle exec checking child after PTY forwarding error");
            if child_gone_at.is_none() {
                let _ = writeln!(
                    std::io::stderr(),
                    "kettle exec: cannot safely write child PTY input: {error}"
                );
                let _ = process_tree.terminate(&term);
                let _ = wait_for_exit_code(&term);
                std::thread::sleep(SETTLE);
                let _ = output_or_stop!(drain_output_slice(
                    &orx,
                    &mut recorder,
                    output,
                    &pty_channel_disconnected,
                    &last_pty_output_at
                ));
                finish_recording(&mut recorder, Duration::ZERO);
                let _ = output.finish(
                    EXIT_INTERNAL,
                    started.elapsed(),
                    OutputFinish::AbandonPending,
                );
                return EXIT_INTERNAL;
            }
            // EIO/BrokenPipe is the normal final write outcome when a
            // short-lived child exits before consuming all piped input. Its
            // authoritative process status wins, but lifecycle handling below
            // must still run this turn.
        }

        if trace_lifecycle {
            log::debug!(
                "kettle exec lifecycle child-status poll returned: exited={}",
                child_gone_at.is_some()
            );
        }
        // Wrap-up needs the PTY to be *finished*, not merely quiet. On Unix the
        // side channel must disconnect *and* the core reader must identify an
        // orderly EOF; an unexpected read failure is an incomplete-output
        // error. Windows retains the bounded ConPTY fallback because its output
        // handle can legitimately outlive the child and final repaint.
        #[allow(unused_mut)] // CloseTransport is native only on Windows
        let (mut pty_drain, pty_drain_generation) = if let Some(gone) = child_gone_at {
            let now = Instant::now();
            let read_progress = term.pty_read_progress();
            if read_progress.generation != observed_read_generation {
                observed_read_generation = read_progress.generation;
                last_pty_source_activity_at.set(now);
            }
            let transport_end =
                observed_pty_transport_end(pty_channel_disconnected.get(), read_progress.status);
            let quiet_since = latest_pty_activity(
                gone,
                last_pty_output_at.get(),
                last_pty_source_activity_at.get(),
            );
            // An accepted stdout command may already be blocked in the OS even
            // when both queues are empty. `drained` observes that outstanding
            // work without joining the writer, so Unix's no-EOF bound and
            // ConPTY's quiet fallback cannot abandon it.
            let output_idle =
                !output_backlog && orx.is_empty() && output_or_stop!(output.drained());
            (
                native_pty_drain_decision(
                    transport_end,
                    now.saturating_duration_since(gone),
                    now.saturating_duration_since(quiet_since),
                    output_idle,
                    read_progress.pending_chunks,
                ),
                Some(read_progress.generation),
            )
        } else {
            (PtyDrainDecision::Wait, None)
        };
        if pty_drain == PtyDrainDecision::CloseTransport {
            #[cfg(windows)]
            {
                if pty_close_started_at.is_none() {
                    match term.process_tree_active_processes() {
                        // The direct child is already gone. Once its Job is
                        // empty, no process remains that can create another
                        // same-console descendant, so closing ConPTY cannot
                        // cut off a later inherited write.
                        Ok(Some(0)) => match term.begin_pty_output_close() {
                            Ok(true) => pty_close_started_at = Some(Instant::now()),
                            Ok(false) => {
                                let _ = writeln!(
                                    std::io::stderr(),
                                    "kettle exec: ConPTY output close was already consumed before EOF"
                                );
                                pty_drain = PtyDrainDecision::Failed(PtyDrainFailure::Read);
                            }
                            Err(error) => {
                                let _ = writeln!(
                                    std::io::stderr(),
                                    "kettle exec: cannot start ConPTY output close: {error}"
                                );
                                pty_drain = PtyDrainDecision::Failed(PtyDrainFailure::Read);
                            }
                        },
                        Ok(Some(_)) => pty_drain = PtyDrainDecision::Wait,
                        Ok(None) => {
                            let _ = writeln!(
                                std::io::stderr(),
                                "kettle exec: ConPTY process-tree containment is unavailable"
                            );
                            pty_drain = PtyDrainDecision::Failed(PtyDrainFailure::Containment);
                        }
                        Err(error) => {
                            let _ = writeln!(
                                std::io::stderr(),
                                "kettle exec: cannot inspect the ConPTY process tree: {error}"
                            );
                            pty_drain = PtyDrainDecision::Failed(PtyDrainFailure::Containment);
                        }
                    }
                }
                if pty_drain == PtyDrainDecision::CloseTransport {
                    pty_drain = PtyDrainDecision::Wait;
                }
            }
            #[cfg(not(windows))]
            unreachable!("only native ConPTY completion closes a quiet transport");
        }
        #[cfg(windows)]
        if pty_drain == PtyDrainDecision::Wait
            && pty_close_started_at.is_some_and(|started| started.elapsed() >= PTY_EOF_TIMEOUT)
        {
            pty_drain = PtyDrainDecision::Failed(PtyDrainFailure::EofTimeout);
        }
        if let PtyDrainDecision::Failed(failure) = pty_drain {
            match failure {
                PtyDrainFailure::Read => {
                    let _ = writeln!(
                        std::io::stderr(),
                        "kettle exec: PTY output reader failed before EOF; output is incomplete"
                    );
                }
                PtyDrainFailure::EofTimeout => {
                    let _ = writeln!(
                        std::io::stderr(),
                        "kettle exec: PTY output did not reach EOF within {}s after the child \
                         exited; output may be incomplete",
                        PTY_EOF_TIMEOUT.as_secs()
                    );
                }
                #[cfg(windows)]
                PtyDrainFailure::Containment => {
                    // The specific Job Object error was emitted at the query
                    // site. Keep common teardown/finalization below.
                }
            }
            let _ = process_tree.terminate(&term);
            let _ = wait_for_exit_code(&term);
            std::thread::sleep(SETTLE);
            drain_recording_slice(&orx, &mut recorder);
            finish_recording(&mut recorder, Duration::ZERO);
            let _ = output.finish(
                EXIT_INTERNAL,
                started.elapsed(),
                OutputFinish::AbandonPending,
            );
            return EXIT_INTERNAL;
        }
        let pty_finished = pty_drain == PtyDrainDecision::Finished;
        if let Some(gone) = child_gone_at
            && gone.elapsed() >= SETTLE
            && orx.is_empty()
            && pty_finished
        {
            // `child_gone_at` is set only by the status observer above, so the
            // code is already authoritative here without consuming the Unix
            // wait status that still anchors Linux teardown.
            let status_ready = child_exit_code.is_some();
            if status_ready {
                // Final drain in case something landed in the settle window.
                // `drained` also waits for every command already admitted to
                // the stdout worker; an empty raw channel alone is insufficient.
                let final_output_backlog = output_or_stop!(drain_output_slice(
                    &orx,
                    &mut recorder,
                    output,
                    &pty_channel_disconnected,
                    &last_pty_output_at
                ));
                if !final_output_backlog && orx.is_empty() && output_or_stop!(output.drained()) {
                    // The first progress snapshot preceded the final raw/output
                    // drains. A ConPTY pump read can race into that interval;
                    // revalidate only after every other completion predicate so
                    // a stale zero-pending snapshot cannot close stdout over a
                    // final repaint hidden in the parser pipeline.
                    let final_progress = term.pty_read_progress();
                    if !pty_completion_snapshot_is_current(pty_drain_generation, final_progress) {
                        observed_read_generation = final_progress.generation;
                        last_pty_source_activity_at.set(Instant::now());
                        continue;
                    }
                    if let Some(recorder) = recorder.as_mut() {
                        recorder.begin_finish();
                        recording_finish_deadline = Some(Instant::now() + RECORD_FINISH_TIMEOUT);
                    } else {
                        recording_finish_deadline = Some(Instant::now());
                    }
                    let code = child_exit_code.unwrap_or(EXIT_INTERNAL);
                    output_or_stop!(
                        output.finish(code, started.elapsed(), OutputFinish::Complete,)
                    );
                    completion_code = Some(code);
                    // Give cancellation/deadline one final turn before
                    // returning, even when a direct in-memory sink finished
                    // synchronously.
                    continue;
                }
            }
        }

        let output_blocked = !output_or_stop!(output.ready());
        if (output_backlog || event_backlog) && !output_blocked {
            // Preserve throughput under a real backlog without paying the idle
            // polling delay, now that lifecycle checks have had a turn.
            std::thread::yield_now();
            continue;
        }
        std::thread::sleep(Duration::from_millis(8));
    }
}

fn pty_completion_snapshot_is_current(
    decision_generation: Option<u64>,
    final_progress: PtyReadProgress,
) -> bool {
    decision_generation == Some(final_progress.generation)
        && final_progress.status == PtyReadStatus::Eof
        && final_progress.pending_chunks == 0
}

fn validate_exec_cwd(cwd: Option<&Path>) -> Result<Option<&str>, String> {
    let Some(cwd) = cwd else {
        return Ok(None);
    };
    let metadata = cwd
        .metadata()
        .map_err(|error| format!("{}: {error}", cwd.display()))?;
    if !metadata.is_dir() {
        return Err(format!("{} is not a directory", cwd.display()));
    }
    cwd.to_str()
        .map(Some)
        .ok_or_else(|| format!("{} is not valid UTF-8", cwd.display()))
}

/// Stop the command's owned process scope for timeout/cancellation.
///
/// Linux freezes and kills the PTY-created session through identity-stable
/// pidfds, while other Unix targets address the PTY process group. Windows
/// containment is established inside the PTY backend before the suspended
/// primary thread resumes, so the portable child kill reaches its Job Object.
struct ExecProcessTree {
    root: Option<u32>,
    #[cfg(target_os = "linux")]
    session: Option<LinuxSession>,
}

impl ExecProcessTree {
    fn attach(term: &Terminal) -> Self {
        let root = term.child_pid();
        #[cfg(target_os = "linux")]
        let session = root.map(LinuxSession::attach);
        Self {
            root,
            #[cfg(target_os = "linux")]
            session,
        }
    }

    /// Stop every process in the owned scope and report whether containment was
    /// proven complete. A timeout/cancellation status is meaningful only when
    /// the command cannot keep running after Kettle returns.
    fn terminate(&self, term: &Terminal) -> bool {
        let Some(root) = self.root.or_else(|| term.child_pid()) else {
            let direct = report_failed_termination(term.kill());
            #[cfg(unix)]
            {
                let _ = direct;
                return false;
            }
            #[cfg(not(unix))]
            return direct;
        };

        let mut complete = true;

        #[cfg(target_os = "linux")]
        {
            let _ = root;
            if let Some(session) = &self.session {
                complete &= session.terminate();
            } else {
                let _ = writeln!(
                    std::io::stderr(),
                    "kettle exec: Linux session containment was not attached; \
                     descendant teardown is unverified"
                );
                complete = false;
            }
        }

        #[cfg(all(unix, not(target_os = "linux")))]
        {
            // portable-pty makes the child the leader of its terminal process
            // group on Unix. A negative pid addresses that entire group.
            if let Ok(group) = libc::pid_t::try_from(root) {
                // SAFETY: group is a positive child PID; negating it selects only
                // that child's process group.
                let result = unsafe { libc::kill(-group, libc::SIGKILL) };
                if result != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        let _ = writeln!(
                            std::io::stderr(),
                            "kettle exec: could not terminate PTY process group {group}: {error}"
                        );
                    }
                    complete = false;
                }
            } else {
                complete = false;
            }
        }

        #[cfg(not(unix))]
        let _ = root;

        log::debug!("kettle exec terminating direct PTY child");
        complete &= report_failed_termination(term.kill());
        log::debug!("kettle exec finished direct PTY child termination");
        complete
    }
}

/// Say so when a child could not be terminated.
///
/// `kettle exec` is about to report a timeout and exit. If the kill genuinely
/// failed the child is still running, which the caller — often an automation
/// harness that will move on to the next command — needs to know. An
/// already-exited child is not a failure and is reported as success by the
/// layer below.
fn report_failed_termination(outcome: std::io::Result<()>) -> bool {
    match outcome {
        Ok(()) => true,
        Err(error) => {
            let _ = writeln!(
                std::io::stderr(),
                "kettle exec: could not terminate the child process; it may still be running: \
                 {error}"
            );
            false
        }
    }
}

#[cfg(target_os = "linux")]
const LINUX_PROC_SCAN_TIME: Duration = Duration::from_millis(500);
#[cfg(target_os = "linux")]
const LINUX_PROC_SCAN_PROCESSES: usize = 16_384;
#[cfg(target_os = "linux")]
struct LinuxProcScanBudget {
    deadline: Instant,
    processes: usize,
    exhausted: bool,
}

#[cfg(target_os = "linux")]
impl LinuxProcScanBudget {
    fn new() -> Self {
        Self {
            deadline: Instant::now() + LINUX_PROC_SCAN_TIME,
            processes: 0,
            exhausted: false,
        }
    }

    fn process(&mut self) -> bool {
        if self.processes >= LINUX_PROC_SCAN_PROCESSES || Instant::now() >= self.deadline {
            self.exhausted = true;
            return false;
        }
        self.processes += 1;
        true
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct LinuxProcessIdentity {
    pid: u32,
    session: u32,
    start_time: u64,
}

#[cfg(target_os = "linux")]
impl LinuxProcessIdentity {
    fn read(pid: u32) -> std::io::Result<Option<Self>> {
        Ok(LinuxProcessStat::read(pid)?.map(|stat| stat.identity))
    }

    fn parse(pid: u32, stat: &str) -> Option<Self> {
        // `comm` is parenthesized and may itself contain spaces or `)`. Split
        // after its final close-paren; fields below are then 3 onward.
        let tail = stat.rsplit_once(") ")?.1;
        let fields = tail.split_ascii_whitespace().collect::<Vec<_>>();
        Some(Self {
            pid,
            session: fields.get(3)?.parse().ok()?,
            start_time: fields.get(19)?.parse().ok()?,
        })
    }
}

#[cfg(target_os = "linux")]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct LinuxProcessStat {
    identity: LinuxProcessIdentity,
    state: u8,
}

#[cfg(target_os = "linux")]
impl LinuxProcessStat {
    fn read(pid: u32) -> std::io::Result<Option<Self>> {
        let stat = match std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let tail = stat
            .rsplit_once(") ")
            .map(|(_, tail)| tail)
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "malformed /proc stat")
            })?;
        let fields = tail.split_ascii_whitespace().collect::<Vec<_>>();
        let state = fields
            .first()
            .and_then(|field| field.as_bytes().first())
            .copied()
            .ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::InvalidData, "missing process state")
            })?;
        let identity = LinuxProcessIdentity::parse(pid, &stat).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "malformed process identity",
            )
        })?;
        Ok(Some(Self { identity, state }))
    }
}

#[cfg(target_os = "linux")]
struct LinuxPidHandle {
    identity: LinuxProcessIdentity,
    fd: std::os::fd::OwnedFd,
}

#[cfg(target_os = "linux")]
impl LinuxPidHandle {
    fn open(identity: LinuxProcessIdentity) -> std::io::Result<Option<Self>> {
        use std::os::fd::FromRawFd as _;

        let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, identity.pid, 0) };
        if raw < 0 {
            let error = std::io::Error::last_os_error();
            return if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(None)
            } else {
                Err(error)
            };
        }
        // SAFETY: pidfd_open returned a new owned descriptor.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw as libc::c_int) };
        if LinuxProcessIdentity::read(identity.pid)? != Some(identity) {
            return Ok(None);
        }
        Ok(Some(Self { identity, fd }))
    }

    fn signal(&self, signal: libc::c_int) -> std::io::Result<bool> {
        use std::os::fd::AsRawFd as _;

        // SAFETY: fd is a live pidfd, null siginfo requests ordinary signal
        // semantics, and flags must be zero for pidfd_send_signal.
        let result = unsafe {
            libc::syscall(
                libc::SYS_pidfd_send_signal,
                self.fd.as_raw_fd(),
                signal,
                std::ptr::null::<libc::siginfo_t>(),
                0,
            )
        };
        if result == 0 {
            Ok(true)
        } else {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(false)
            } else {
                Err(error)
            }
        }
    }

    fn stopped_or_gone(&self) -> std::io::Result<bool> {
        let Some(stat) = LinuxProcessStat::read(self.identity.pid)? else {
            return Ok(true);
        };
        if stat.identity != self.identity {
            return Ok(true);
        }
        Ok(matches!(stat.state, b'T' | b't' | b'Z' | b'X'))
    }
}

#[cfg(target_os = "linux")]
struct LinuxSession {
    id: u32,
    process_group: u32,
    root_identity: Option<LinuxProcessIdentity>,
    root: Option<LinuxPidHandle>,
    attach_warning: Option<String>,
}

#[cfg(target_os = "linux")]
impl LinuxSession {
    fn attach(root: u32) -> Self {
        let mut warning = None;
        let identity = match LinuxProcessIdentity::read(root) {
            Ok(identity) => identity,
            Err(error) => {
                warning = Some(format!("cannot read /proc/{root}/stat: {error}"));
                None
            }
        };
        let id = identity.map_or(root, |identity| identity.session);
        let root_handle = match identity {
            Some(identity) => match LinuxPidHandle::open(identity) {
                Ok(Some(handle)) => Some(handle),
                Ok(None) => {
                    warning = Some(format!(
                        "child {root} disappeared before its pidfd could be retained"
                    ));
                    None
                }
                Err(error) => {
                    warning = Some(format!("cannot open pidfd for child {root}: {error}"));
                    None
                }
            },
            None => None,
        };
        Self {
            id,
            process_group: root,
            root_identity: identity,
            root: root_handle,
            attach_warning: warning,
        }
    }

    fn terminate(&self) -> bool {
        let mut budget = LinuxProcScanBudget::new();
        let mut handles = std::collections::HashMap::new();
        let mut incomplete = false;
        if let Some(warning) = &self.attach_warning {
            incomplete = true;
            let _ = writeln!(
                std::io::stderr(),
                "kettle exec: Linux session cleanup is degraded: {warning}"
            );
        }
        let Some(root) = &self.root else {
            let _ = writeln!(
                std::io::stderr(),
                "kettle exec: Linux session cleanup cannot authenticate the original leader; \
                 refusing numeric session/process-group signals"
            );
            return false;
        };
        match root.fd.try_clone() {
            Ok(fd) => {
                handles.insert(
                    root.identity,
                    LinuxPidHandle {
                        identity: root.identity,
                        fd,
                    },
                );
            }
            Err(error) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "kettle exec: could not retain child pidfd for cleanup: {error}; \
                     refusing numeric session/process-group signals"
                );
                return false;
            }
        }

        // Stop the original leader through its pidfd first, then prove that
        // the exact start-time identity remains present. While that stopped
        // process (or its unreaped zombie) exists, Linux cannot recycle its PID
        // as a new session or process-group id. Without this anchor a numeric
        // scan or kill could reach a newly created, unrelated session.
        let root_stopped = match root.signal(libc::SIGSTOP) {
            Ok(signal_delivered) => {
                let deadline = Instant::now() + Duration::from_millis(100);
                loop {
                    match LinuxProcessStat::read(root.identity.pid) {
                        Ok(current)
                            if linux_root_anchors_numeric_scope(self.root_identity, current) =>
                        {
                            break true;
                        }
                        Ok(Some(current)) if current.identity == root.identity => {
                            // An unreaped exited child is already frozen and
                            // still reserves its PID/session ids, but Linux
                            // correctly reports ESRCH when asked to signal the
                            // zombie through its pidfd. A live identity for
                            // which no signal was delivered is not an anchor.
                            if !signal_delivered {
                                break false;
                            }
                        }
                        Ok(_) => break false,
                        Err(error) => {
                            let _ = writeln!(
                                std::io::stderr(),
                                "kettle exec: cannot revalidate Linux session leader: {error}"
                            );
                            break false;
                        }
                    }
                    if Instant::now() >= deadline {
                        break false;
                    }
                    std::thread::sleep(Duration::from_millis(2));
                }
            }
            Err(error) => {
                let _ = writeln!(
                    std::io::stderr(),
                    "kettle exec: cannot stop Linux session leader through pidfd: {error}"
                );
                false
            }
        };
        if !root_stopped {
            let _ = writeln!(
                std::io::stderr(),
                "kettle exec: Linux session cleanup cannot prove the original leader still \
                 anchors its numeric ids; refusing session/process-group signals"
            );
            return false;
        }

        // Cover the PTY's original process group even when procfs or pidfds
        // are unavailable. This cannot reach a member that deliberately made a
        // new group, but it is identity-safe: an existing process group keeps
        // its numeric id reserved until its final member exits.
        if !linux_signal_process_group(self.process_group, libc::SIGSTOP) {
            incomplete = true;
        }

        // Session membership is inherited and cannot be joined by an
        // unrelated process. Unlike ancestry it survives reparenting, and a
        // full /proc scan also sees children created by non-leader threads.
        // Hold pidfds before signaling so numeric PID reuse cannot redirect a
        // stop or kill to a different process.
        let mut stable = false;
        for _ in 0..8 {
            let before = handles.len();
            let scan = linux_session_processes(self.id, 4096, &mut budget);
            incomplete |= scan.incomplete;
            for handle in scan.handles {
                if let std::collections::hash_map::Entry::Vacant(entry) =
                    handles.entry(handle.identity)
                {
                    if let Err(error) = handle.signal(libc::SIGSTOP) {
                        incomplete = true;
                        let _ = writeln!(
                            std::io::stderr(),
                            "kettle exec: could not stop Linux session member {}: {error}",
                            handle.identity.pid
                        );
                    }
                    entry.insert(handle);
                }
            }

            // SIGSTOP delivery is asynchronous. Do not call a scan stable
            // until every retained identity is observed stopped (or gone),
            // because a still-running member can fork after the final scan.
            let stop_deadline =
                std::cmp::min(budget.deadline, Instant::now() + Duration::from_millis(100));
            let all_stopped = loop {
                let mut all_stopped = true;
                for handle in handles.values() {
                    match handle.stopped_or_gone() {
                        Ok(true) => {}
                        Ok(false) => all_stopped = false,
                        Err(error) => {
                            incomplete = true;
                            all_stopped = false;
                            let _ = writeln!(
                                std::io::stderr(),
                                "kettle exec: could not confirm Linux session member {} stopped: {error}",
                                handle.identity.pid
                            );
                        }
                    }
                }
                if all_stopped || Instant::now() >= stop_deadline {
                    break all_stopped;
                }
                std::thread::sleep(Duration::from_millis(2));
            };
            if all_stopped && handles.len() == before {
                stable = true;
                break;
            }
            if budget.exhausted {
                break;
            }
        }
        if budget.exhausted || !stable || incomplete {
            let _ = writeln!(
                std::io::stderr(),
                "kettle exec: Linux process cleanup could not prove the complete session was \
                 frozen; falling back to the PTY process group, and a member in another group \
                 may still be running"
            );
        }
        for handle in handles.values() {
            match handle.signal(libc::SIGKILL) {
                Ok(_) => {}
                Err(error) => {
                    incomplete = true;
                    let _ = handle.signal(libc::SIGCONT);
                    let _ = writeln!(
                        std::io::stderr(),
                        "kettle exec: could not kill Linux session member {}: {error}",
                        handle.identity.pid
                    );
                }
            }
        }
        if !linux_signal_process_group(self.process_group, libc::SIGKILL) {
            incomplete = true;
        }
        stable && !incomplete
    }
}

#[cfg(target_os = "linux")]
fn linux_signal_process_group(group: u32, signal: libc::c_int) -> bool {
    let Ok(group) = libc::pid_t::try_from(group) else {
        return false;
    };
    let result = unsafe { libc::kill(-group, signal) };
    if result == 0 {
        true
    } else {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            // No member remains in this group. The retained root identity and
            // pidfd-backed session scan provide the ownership proof; an empty
            // fallback scope is already terminated.
            return true;
        }
        let _ = writeln!(
            std::io::stderr(),
            "kettle exec: could not signal Linux PTY process group {group}: {error}"
        );
        false
    }
}

#[cfg(target_os = "linux")]
fn linux_root_anchors_numeric_scope(
    expected: Option<LinuxProcessIdentity>,
    current: Option<LinuxProcessStat>,
) -> bool {
    matches!(
        (expected, current),
        (Some(expected), Some(current))
            if current.identity == expected && matches!(current.state, b'T' | b't' | b'Z' | b'X')
    )
}

#[cfg(target_os = "linux")]
struct LinuxSessionScan {
    handles: Vec<LinuxPidHandle>,
    incomplete: bool,
}

#[cfg(target_os = "linux")]
fn linux_session_processes(
    session: u32,
    limit: usize,
    budget: &mut LinuxProcScanBudget,
) -> LinuxSessionScan {
    if limit == 0 {
        budget.exhausted = true;
        return LinuxSessionScan {
            handles: Vec::new(),
            incomplete: true,
        };
    }
    let processes = match std::fs::read_dir("/proc") {
        Ok(processes) => processes,
        Err(_) => {
            return LinuxSessionScan {
                handles: Vec::new(),
                incomplete: true,
            };
        }
    };
    let own_pid = std::process::id();
    let mut members = Vec::new();
    let mut incomplete = false;
    for process in processes {
        let process = match process {
            Ok(process) => process,
            Err(_) => {
                incomplete = true;
                continue;
            }
        };
        if !budget.process() {
            break;
        }
        let Some(pid) = process
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == own_pid {
            continue;
        }
        let identity = match LinuxProcessIdentity::read(pid) {
            Ok(Some(identity)) => identity,
            Ok(None) => continue,
            Err(_) => {
                incomplete = true;
                continue;
            }
        };
        if identity.session == session {
            match LinuxPidHandle::open(identity) {
                Ok(Some(handle)) => {
                    members.push(handle);
                    if members.len() >= limit {
                        budget.exhausted = true;
                        break;
                    }
                }
                Ok(None) => {}
                Err(_) => incomplete = true,
            }
        }
    }
    LinuxSessionScan {
        handles: members,
        incomplete,
    }
}

fn record_chunk(recorder: &mut Option<kettle_core::record::Recorder>, bytes: &[u8]) {
    let Some(recorder) = recorder.as_mut() else {
        return;
    };
    recorder.record_output(bytes);
    report_recording_status(recorder);
}

fn report_recording_status(recorder: &mut kettle_core::record::Recorder) {
    use kettle_core::record::RecordStatus;

    let Some(status) = recorder.take_status_change() else {
        return;
    };
    let reason = match status {
        RecordStatus::LimitReached => "512 MiB session limit reached",
        RecordStatus::Overloaded => "bounded persistence queue filled; the asciicast is incomplete",
        RecordStatus::IoError => "recording I/O failed or finalization exceeded its bound",
        RecordStatus::Recording => return,
    };
    let _ = writeln!(
        std::io::stderr(),
        "kettle exec: asciicast capture stopped ({reason}); the trace is incomplete"
    );
}

fn finish_recording(recorder: &mut Option<kettle_core::record::Recorder>, timeout: Duration) {
    if let Some(mut recorder) = recorder.take() {
        // `finish_with_timeout` polls `is_finished` and joins only an exited
        // worker. A filesystem call that never returns therefore consumes this
        // explicit grace, never the lifecycle thread's remaining lifetime.
        let _ = recorder.finish_with_timeout(timeout);
        report_recording_status(&mut recorder);
    }
}

fn poll_recording_finish(
    recorder: &mut Option<kettle_core::record::Recorder>,
    deadline: Instant,
) -> bool {
    let Some(active) = recorder.as_mut() else {
        return true;
    };
    if active.try_finish() {
        report_recording_status(active);
        recorder.take();
        return true;
    }
    report_recording_status(active);
    if Instant::now() < deadline {
        return false;
    }
    // A zero-duration final poll marks a writer still inside the OS as failed
    // and detaches it. The CLI reports that failure before returning, while a
    // healthy worker is joined only after `is_finished` proved it safe.
    let mut active = recorder.take().expect("recorder was present above");
    let _ = active.finish_with_timeout(Duration::ZERO);
    report_recording_status(&mut active);
    true
}

/// Map a child's raw exit code into the code this process should report.
///
/// On Unix `std::process::exit` only keeps the low 8 bits, and portable-pty
/// folds signal death into the code there, so we mask to 0..=255 — the value we
/// log then matches what the shell would see. On Windows the full 32-bit code
/// is meaningful (children routinely exit with codes outside 0..=255, e.g.
/// `STATUS_ACCESS_VIOLATION` 0xC0000005), so we reinterpret the bits rather
/// than truncating or saturating.
///
/// Saturating was wrong for exactly the case the line above names: it turned
/// 0xC0000005 into 0x7FFFFFFF and destroyed the diagnostic. `as i32` preserves
/// every bit, and Windows takes the low 32 bits back off the process exit, so
/// the caller sees the status the child really died with.
fn clamp_code(code: u32) -> i32 {
    #[cfg(unix)]
    {
        (code & 0xff) as i32
    }
    #[cfg(windows)]
    {
        code as i32
    }
    #[cfg(not(any(unix, windows)))]
    {
        (code & 0xff) as i32
    }
}

/// Poll the child's exit status for up to ~250 ms after its VT `Exit` event, to
/// cover the brief window before the OS reaps it. `None` only if it never
/// materializes (then the caller reports a non-zero sentinel, never a false 0).
fn wait_for_exit_code(term: &Terminal) -> Option<u32> {
    let deadline = Instant::now() + CHILD_EXIT_STATUS_WAIT;
    loop {
        if let Some(c) = term.child_exit_code() {
            return Some(c);
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Decode the longest valid UTF-8 prefix of `carry` into `out`, retaining a
/// genuinely-incomplete trailing sequence in `carry` for the next call (so a
/// codepoint split across two PTY reads isn't mangled into U+FFFD halves);
/// genuinely-invalid bytes become one U+FFFD.
fn push_utf8_streaming(carry: &mut Vec<u8>, bytes: &[u8], out: &mut String) {
    carry.extend_from_slice(bytes);
    loop {
        match std::str::from_utf8(carry) {
            Ok(s) => {
                out.push_str(s);
                carry.clear();
                return;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                // SAFETY: bytes up to `valid` are valid UTF-8.
                out.push_str(unsafe { std::str::from_utf8_unchecked(&carry[..valid]) });
                match e.error_len() {
                    None => {
                        carry.drain(..valid);
                        return;
                    }
                    Some(n) => {
                        out.push('\u{FFFD}');
                        carry.drain(..valid + n);
                    }
                }
            }
        }
    }
}

#[derive(Clone, Copy)]
enum OutputFinish {
    /// Ordinary completion is lossless: enqueue a final flush after every
    /// admitted command has completed, then poll it from lifecycle turns.
    Complete,
    /// Timeout, cancellation, and fatal teardown must not wait for a stalled
    /// stdout consumer.
    AbandonPending,
}

trait ExecOutput {
    /// Try to publish the one lifecycle-owned pending command.
    fn ready(&mut self) -> OutputResult<bool>;
    /// Whether every command admitted before this call has completed.
    fn drained(&mut self) -> OutputResult<bool> {
        self.ready()
    }
    /// Whether a previously requested complete finish has flushed and joined.
    /// Direct sinks finish synchronously; worker-backed sinks override this.
    fn completion_ready(&mut self) -> OutputResult<bool> {
        Ok(true)
    }
    fn start(&mut self, cols: u16, rows: u16) -> OutputResult<()>;
    fn output(&mut self, bytes: Vec<u8>) -> OutputResult<()>;
    fn title(&mut self, title: String) -> OutputResult<()>;
    fn finish(&mut self, code: i32, duration: Duration, mode: OutputFinish) -> OutputResult<()>;
}

struct DirectOutput<'a> {
    outputter: Outputter,
    sink: &'a mut dyn Write,
}

impl<'a> DirectOutput<'a> {
    fn new(mode: OutputMode, sink: &'a mut dyn Write) -> Self {
        Self {
            outputter: Outputter::new(mode),
            sink,
        }
    }
}

impl ExecOutput for DirectOutput<'_> {
    fn ready(&mut self) -> OutputResult<bool> {
        Ok(true)
    }

    fn start(&mut self, cols: u16, rows: u16) -> OutputResult<()> {
        self.outputter
            .start(self.sink, cols, rows)
            .map_err(Into::into)
    }

    fn output(&mut self, bytes: Vec<u8>) -> OutputResult<()> {
        self.outputter.output(self.sink, &bytes).map_err(Into::into)
    }

    fn title(&mut self, title: String) -> OutputResult<()> {
        self.outputter.title(self.sink, &title).map_err(Into::into)
    }

    fn finish(&mut self, code: i32, duration: Duration, _mode: OutputFinish) -> OutputResult<()> {
        self.outputter
            .finish(self.sink, code, duration)
            .map_err(Into::into)
    }
}

enum OutputCommand {
    Start { cols: u16, rows: u16 },
    Output(Vec<u8>),
    Title(String),
    Finish { code: i32, duration: Duration },
}

/// Own stdout on a dedicated bounded worker so an OS-level `write` cannot park
/// the PTY lifecycle thread. At most the channel plus `pending` are retained;
/// once full, the lifecycle stops draining the lossless PTY output channel and
/// lets bounded backpressure propagate to the child.
struct WorkerOutput {
    mode: OutputMode,
    sender: Option<Sender<OutputCommand>>,
    pending: Option<OutputCommand>,
    outstanding: Arc<AtomicUsize>,
    completion_started: bool,
    worker: Option<std::thread::JoinHandle<()>>,
    outcome_rx: Receiver<OutputResult<()>>,
    outcome: Option<OutputResult<()>>,
}

/// `main` restores the conventional Unix SIGPIPE disposition for ordinary CLI
/// writes. The exec stdout worker is different: it must observe `EPIPE`, report
/// exit 74, and reap the PTY child instead of letting the signal kill Kettle.
#[cfg(unix)]
fn block_sigpipe_for_current_thread() -> std::io::Result<()> {
    // SAFETY: sigemptyset/sigaddset initialize and mutate only this local set;
    // pthread_sigmask applies it to the calling writer thread.
    unsafe {
        let mut signals: libc::sigset_t = std::mem::zeroed();
        if libc::sigemptyset(&mut signals) != 0 || libc::sigaddset(&mut signals, libc::SIGPIPE) != 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let error = libc::pthread_sigmask(libc::SIG_BLOCK, &signals, std::ptr::null_mut());
        if error == 0 {
            Ok(())
        } else {
            Err(std::io::Error::from_raw_os_error(error))
        }
    }
}

#[cfg(not(unix))]
fn block_sigpipe_for_current_thread() -> std::io::Result<()> {
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "android"))]
fn consume_pending_sigpipe(error: &std::io::Error) {
    if error.kind() != std::io::ErrorKind::BrokenPipe {
        return;
    }
    // A blocked synchronous SIGPIPE remains pending after write returns EPIPE.
    // Consume it before this thread exits; otherwise pthread teardown can make
    // the default disposition observable after the lifecycle already chose 74.
    // SAFETY: all pointers refer to initialized local signal/timespec values,
    // and SIGPIPE is blocked on this thread before any output write occurs.
    unsafe {
        let mut signals: libc::sigset_t = std::mem::zeroed();
        if libc::sigemptyset(&mut signals) != 0 || libc::sigaddset(&mut signals, libc::SIGPIPE) != 0
        {
            return;
        }
        let timeout = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        loop {
            let signal = libc::sigtimedwait(&signals, std::ptr::null_mut(), &timeout);
            if signal == libc::SIGPIPE {
                return;
            }
            if signal == -1
                && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted
            {
                continue;
            }
            return;
        }
    }
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "android"))))]
fn consume_pending_sigpipe(error: &std::io::Error) {
    if error.kind() != std::io::ErrorKind::BrokenPipe {
        return;
    }
    // macOS and the BSDs do not expose sigtimedwait. Check the blocked set
    // first so a synthetic BrokenPipe cannot make sigwait block forever, then
    // synchronously consume a real pending SIGPIPE before this thread exits.
    // SAFETY: all pointers refer to initialized local signal sets/values, and
    // SIGPIPE is blocked on this thread before any output write occurs.
    unsafe {
        let mut signals: libc::sigset_t = std::mem::zeroed();
        if libc::sigemptyset(&mut signals) != 0 || libc::sigaddset(&mut signals, libc::SIGPIPE) != 0
        {
            return;
        }
        let mut pending: libc::sigset_t = std::mem::zeroed();
        if libc::sigpending(&mut pending) != 0 || libc::sigismember(&pending, libc::SIGPIPE) != 1 {
            return;
        }
        let mut signal = 0;
        loop {
            let wait_error = libc::sigwait(&signals, &mut signal);
            if wait_error == 0 {
                return;
            }
            if wait_error != libc::EINTR {
                return;
            }
        }
    }
}

#[cfg(not(unix))]
fn consume_pending_sigpipe(_error: &std::io::Error) {}

/// Retire one command from the stdout worker.
///
/// Failure publication must precede the acknowledgement that drops
/// `outstanding` to zero. The lifecycle samples that counter to decide whether
/// an already-admitted write has drained; publishing in the opposite order
/// creates a window where a failed write looks successfully complete.
fn retire_output_command(
    outstanding: &AtomicUsize,
    result: std::io::Result<()>,
    finished: bool,
    publish_outcome: impl FnOnce(OutputResult<()>),
) -> bool {
    match result {
        Ok(()) => {
            let previous = outstanding.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous != 0, "stdout command was not tracked");
            if finished {
                publish_outcome(Ok(()));
            }
            finished
        }
        Err(error) => {
            consume_pending_sigpipe(&error);
            publish_outcome(Err(error.into()));
            let previous = outstanding.fetch_sub(1, Ordering::AcqRel);
            debug_assert!(previous != 0, "stdout command was not tracked");
            true
        }
    }
}

impl WorkerOutput {
    fn spawn(mode: OutputMode, mut sink: impl Write + Send + 'static) -> std::io::Result<Self> {
        let (sender, receiver) = crossbeam_channel::bounded(OUTPUT_WRITER_QUEUE_DEPTH);
        let (outcome_tx, outcome_rx) = crossbeam_channel::bounded(1);
        let outstanding = Arc::new(AtomicUsize::new(0));
        let worker_outstanding = Arc::clone(&outstanding);
        let worker = std::thread::Builder::new()
            .name("kettle-stdout-writer".into())
            .spawn(move || match block_sigpipe_for_current_thread() {
                Err(error) => {
                    let _ = outcome_tx.send(Err(error.into()));
                }
                Ok(()) => {
                    let mut outputter = Outputter::new(mode);
                    loop {
                        let command = match receiver.recv() {
                            Ok(command) => command,
                            Err(_) => {
                                let _ = outcome_tx.send(Err(OutputDeliveryError::unexpected(
                                    "stdout writer command channel closed before completion",
                                )));
                                break;
                            }
                        };
                        let (finished, result) = match command {
                            OutputCommand::Start { cols, rows } => {
                                (false, outputter.start(&mut sink, cols, rows))
                            }
                            OutputCommand::Output(bytes) => {
                                (false, outputter.output(&mut sink, &bytes))
                            }
                            OutputCommand::Title(title) => {
                                (false, outputter.title(&mut sink, &title))
                            }
                            OutputCommand::Finish { code, duration } => {
                                (true, outputter.finish(&mut sink, code, duration))
                            }
                        };
                        if retire_output_command(&worker_outstanding, result, finished, |outcome| {
                            let _ = outcome_tx.send(outcome);
                        }) {
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            mode,
            sender: Some(sender),
            pending: None,
            outstanding,
            completion_started: false,
            worker: Some(worker),
            outcome_rx,
            outcome: None,
        })
    }

    fn poll_worker_outcome(&mut self) -> OutputResult<bool> {
        if self.outcome.is_none() {
            match self.outcome_rx.try_recv() {
                Ok(outcome) => self.outcome = Some(outcome),
                Err(crossbeam_channel::TryRecvError::Empty) => {}
                Err(crossbeam_channel::TryRecvError::Disconnected) => {
                    self.outcome = Some(Err(OutputDeliveryError::unexpected(
                        "stdout writer stopped without reporting an outcome",
                    )));
                }
            }
        }
        match self.outcome.as_ref() {
            Some(Ok(())) => Ok(true),
            Some(Err(error)) => Err(error.clone()),
            None => Ok(false),
        }
    }

    fn dispatch(&mut self, command: OutputCommand) -> OutputResult<bool> {
        debug_assert!(self.pending.is_none());
        if self.poll_worker_outcome()? {
            return Err(OutputDeliveryError::unexpected(
                "stdout writer completed before accepting all output",
            ));
        }
        let Some(sender) = self.sender.as_ref() else {
            return Err(OutputDeliveryError::unexpected(
                "stdout writer is no longer available",
            ));
        };
        self.outstanding.fetch_add(1, Ordering::AcqRel);
        match sender.try_send(command) {
            Ok(()) => Ok(true),
            Err(crossbeam_channel::TrySendError::Full(command)) => {
                self.outstanding.fetch_sub(1, Ordering::AcqRel);
                self.pending = Some(command);
                Ok(false)
            }
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                self.outstanding.fetch_sub(1, Ordering::AcqRel);
                match self.poll_worker_outcome() {
                    Err(error) => Err(error),
                    _ => Err(OutputDeliveryError::unexpected(
                        "stdout writer stopped before accepting all output",
                    )),
                }
            }
        }
    }

    fn enqueue(&mut self, command: OutputCommand) -> OutputResult<()> {
        let _ = self.dispatch(command)?;
        Ok(())
    }

    fn finish_complete(&mut self, code: i32, duration: Duration) -> OutputResult<()> {
        debug_assert!(self.pending.is_none());
        debug_assert!(!self.completion_started);
        self.completion_started = true;
        self.enqueue(OutputCommand::Finish { code, duration })
    }

    fn finish_abandoning_pending(&mut self, code: i32, duration: Duration) -> OutputResult<()> {
        let dispatch_result = if !self.completion_started && self.pending.is_none() {
            self.enqueue(OutputCommand::Finish { code, duration })
        } else {
            self.poll_worker_outcome().map(|_| ())
        };

        // Say so on stderr rather than only in a debug log. The exit code here
        // is the child's own when it was collected, so a caller that reads only
        // the status cannot otherwise tell a fully delivered run from one whose
        // tail was dropped because the caller's own reader stalled.
        if dispatch_result.is_ok()
            && (self.pending.is_some() || self.outstanding.load(Ordering::Acquire) != 0)
        {
            let _ = writeln!(
                std::io::stderr(),
                "kettle exec: stdout was not fully delivered before the run stopped; \
                 the consumer had not accepted all output"
            );
        }

        // Chosen timeout/cancellation contract: commands already accepted by
        // the worker may complete if the consumer resumes immediately, but the
        // lifecycle never waits. The lifecycle-owned pending command, any raw
        // PTY tail not admitted to stdout, and a final JSON exit event that
        // cannot enter the full queue are abandoned explicitly. `main` then
        // calls `process::exit`, which terminates a writer still blocked in the
        // OS. Ordinary completion polls acknowledgements and the final worker
        // exit from the lifecycle loop, so it remains lossless without hiding
        // a later deadline or cancellation.
        self.pending = None;
        drop(self.sender.take());
        drop(self.worker.take());
        dispatch_result
    }
}

impl ExecOutput for WorkerOutput {
    fn ready(&mut self) -> OutputResult<bool> {
        if self.poll_worker_outcome()? {
            return if self.completion_started {
                Ok(true)
            } else {
                Err(OutputDeliveryError::unexpected(
                    "stdout writer completed before the output stream",
                ))
            };
        }
        let Some(command) = self.pending.take() else {
            return Ok(true);
        };
        self.dispatch(command)
    }

    fn drained(&mut self) -> OutputResult<bool> {
        if !self.ready()? {
            return Ok(false);
        }
        // Load before polling the outcome. On failure the worker publishes the
        // error before its release decrement; observing zero here therefore
        // makes that error visible to the following poll rather than allowing
        // one false `Ok(true)` sample between the two operations.
        let outstanding = self.outstanding.load(Ordering::Acquire);
        let _ = self.poll_worker_outcome()?;
        Ok(outstanding == 0)
    }

    fn completion_ready(&mut self) -> OutputResult<bool> {
        if !self.poll_worker_outcome()? {
            return Ok(false);
        }
        let Some(worker) = self.worker.as_ref() else {
            return Ok(true);
        };
        if !worker.is_finished() {
            return Ok(false);
        }
        drop(self.sender.take());
        if self
            .worker
            .take()
            .is_some_and(|worker| worker.join().is_err())
        {
            return Err(OutputDeliveryError::unexpected(
                "stdout writer panicked after reporting completion",
            ));
        }
        Ok(true)
    }

    fn start(&mut self, cols: u16, rows: u16) -> OutputResult<()> {
        if self.mode == OutputMode::Json {
            self.enqueue(OutputCommand::Start { cols, rows })?;
        }
        Ok(())
    }

    fn output(&mut self, bytes: Vec<u8>) -> OutputResult<()> {
        self.enqueue(OutputCommand::Output(bytes))
    }

    fn title(&mut self, title: String) -> OutputResult<()> {
        if self.mode == OutputMode::Json {
            self.enqueue(OutputCommand::Title(title))?;
        }
        Ok(())
    }

    fn finish(&mut self, code: i32, duration: Duration, mode: OutputFinish) -> OutputResult<()> {
        match mode {
            OutputFinish::Complete => self.finish_complete(code, duration),
            OutputFinish::AbandonPending => self.finish_abandoning_pending(code, duration),
        }
    }
}

/// Render child output to stdout in the selected mode.
struct Outputter {
    mode: OutputMode,
    stripper: AnsiStripper,
    scratch: Vec<u8>,
    /// Carry for an incomplete multibyte UTF-8 sequence split across reads
    /// (JSON mode, where output is decoded to a string per chunk).
    utf8_carry: Vec<u8>,
}

impl Outputter {
    fn new(mode: OutputMode) -> Self {
        Self {
            mode,
            stripper: AnsiStripper::default(),
            scratch: Vec::with_capacity(8192),
            utf8_carry: Vec::new(),
        }
    }

    fn start(&mut self, sink: &mut dyn Write, cols: u16, rows: u16) -> std::io::Result<()> {
        if self.mode == OutputMode::Json {
            let v = serde_json::json!({"v":1,"event":"start","cols":cols,"rows":rows});
            writeln!(sink, "{v}")?;
            sink.flush()?;
        }
        Ok(())
    }

    fn output(&mut self, sink: &mut dyn Write, bytes: &[u8]) -> std::io::Result<()> {
        match self.mode {
            OutputMode::Raw => {
                sink.write_all(bytes)?;
                sink.flush()?;
            }
            OutputMode::StripAnsi => {
                self.scratch.clear();
                self.stripper.push(bytes, &mut self.scratch);
                sink.write_all(&self.scratch)?;
                sink.flush()?;
            }
            OutputMode::Json => {
                let mut data = String::new();
                push_utf8_streaming(&mut self.utf8_carry, bytes, &mut data);
                if data.is_empty() {
                    return Ok(()); // only an incomplete sequence so far — wait for more
                }
                let v = serde_json::json!({"v":1,"event":"output","data":data});
                writeln!(sink, "{v}")?;
                sink.flush()?;
            }
        }
        Ok(())
    }

    fn title(&mut self, sink: &mut dyn Write, title: &str) -> std::io::Result<()> {
        if self.mode == OutputMode::Json {
            let v = serde_json::json!({"v":1,"event":"title","data":title});
            writeln!(sink, "{v}")?;
            sink.flush()?;
        }
        Ok(())
    }

    fn finish(&mut self, sink: &mut dyn Write, code: i32, dur: Duration) -> std::io::Result<()> {
        if self.mode == OutputMode::Json {
            // v2.27.0 (audit): flush any trailing incomplete UTF-8 sequence
            // lossily before the exit event, so a stream that ends mid-codepoint
            // doesn't silently drop its final bytes.
            if !self.utf8_carry.is_empty() {
                let data = String::from_utf8_lossy(&self.utf8_carry).into_owned();
                self.utf8_carry.clear();
                let v = serde_json::json!({"v":1,"event":"output","data":data});
                writeln!(sink, "{v}")?;
            }
            let v = serde_json::json!({
                "v":1,"event":"exit","code":code,"duration_ms":dur.as_millis() as u64
            });
            writeln!(sink, "{v}")?;
        }
        sink.flush()
    }
}

enum StdinPumpEvent {
    Data(Vec<u8>),
    Eof,
    ReadError(String),
}

enum StdinPumpResult {
    Eof(Result<bool, String>),
    ReadError(String),
    ForwardError(String),
}

struct PendingWrite {
    bytes: Vec<u8>,
    offset: usize,
}

impl PendingWrite {
    fn new(bytes: Vec<u8>) -> Option<Self> {
        (!bytes.is_empty()).then_some(Self { bytes, offset: 0 })
    }

    fn remaining(&self) -> &[u8] {
        &self.bytes[self.offset..]
    }

    fn advance(&mut self, count: usize) -> bool {
        self.offset += count;
        self.offset == self.bytes.len()
    }
}

fn priority_reply_may_start(
    reply_current: &Option<PendingWrite>,
    stdin_current: &Option<PendingWrite>,
) -> bool {
    reply_current.is_none() && stdin_current.is_none()
}

fn process_stdin_event(
    event: StdinPumpEvent,
    current: &mut Option<PendingWrite>,
    stdin_open: &mut bool,
    eof_pending: &mut bool,
    done: &Sender<StdinPumpResult>,
) {
    match event {
        StdinPumpEvent::Data(bytes) => {
            debug_assert!(current.is_none());
            *current = PendingWrite::new(bytes);
        }
        StdinPumpEvent::Eof => {
            *eof_pending = true;
            *stdin_open = false;
        }
        StdinPumpEvent::ReadError(error) => {
            let _ = done.send(StdinPumpResult::ReadError(error));
            *stdin_open = false;
        }
    }
}

/// Run at most one low-priority EOF step after a final reply recheck.
///
/// The caller's initial empty-channel observation is only a fast path. Reply
/// publication uses the same gate, so an already-admitted reply is either
/// loaded into `reply_current` here or was published after the nonblocking EOF
/// step completed. This cannot give a future child query priority over a VEOF
/// byte the kernel already accepted.
fn try_eof_after_reply_recheck<T>(
    reply_gate: &Mutex<()>,
    replies: &Receiver<Vec<u8>>,
    replies_open: &mut bool,
    reply_current: &mut Option<PendingWrite>,
    eof_step: impl FnOnce() -> T,
) -> Option<T> {
    let _gate = reply_gate.lock().unwrap_or_else(PoisonError::into_inner);
    if reply_current.is_none() && *replies_open {
        match replies.try_recv() {
            Ok(bytes) => *reply_current = PendingWrite::new(bytes),
            Err(crossbeam_channel::TryRecvError::Empty) => {}
            Err(crossbeam_channel::TryRecvError::Disconnected) => {
                *replies_open = false;
            }
        }
    }
    (reply_current.is_none() && replies.is_empty()).then(eof_step)
}

fn spawn_pty_writer_arbiter(
    mut pty_stdin: PtyStdin,
    replies: Receiver<Vec<u8>>,
    reply_gate: Arc<Mutex<()>>,
    stdin_events: Receiver<StdinPumpEvent>,
    done: Sender<StdinPumpResult>,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name("kettle-pty-writer".into())
        .spawn(move || {
            let mut reply_current = None;
            let mut stdin_current = None;
            let mut replies_open = true;
            let mut stdin_open = true;
            let mut eof_pending = false;
            let never_reply = crossbeam_channel::never::<Vec<u8>>();
            let never_stdin = crossbeam_channel::never::<StdinPumpEvent>();

            loop {
                if priority_reply_may_start(&reply_current, &stdin_current) && replies_open {
                    match replies.try_recv() {
                        Ok(bytes) => reply_current = PendingWrite::new(bytes),
                        Err(crossbeam_channel::TryRecvError::Empty) => {}
                        Err(crossbeam_channel::TryRecvError::Disconnected) => {
                            replies_open = false;
                        }
                    }
                }
                if stdin_current.is_none() && stdin_open {
                    match stdin_events.try_recv() {
                        Ok(event) => process_stdin_event(
                            event,
                            &mut stdin_current,
                            &mut stdin_open,
                            &mut eof_pending,
                            &done,
                        ),
                        Err(crossbeam_channel::TryRecvError::Empty) => {}
                        Err(crossbeam_channel::TryRecvError::Disconnected) => stdin_open = false,
                    }
                }

                if eof_pending
                    && reply_current.is_none()
                    && replies.is_empty()
                    && stdin_current.is_none()
                {
                    // Close the check/write race with reply publication. The
                    // producer holds the same gate only around `try_send`, and
                    // this worker holds it only through one nonblocking VEOF
                    // step, so neither PTY capacity nor a slow child can extend
                    // the critical section.
                    let mut eof_retry_pending = false;
                    if let Some(progress) = try_eof_after_reply_recheck(
                        &reply_gate,
                        &replies,
                        &mut replies_open,
                        &mut reply_current,
                        || pty_stdin.try_signal_eof(),
                    ) {
                        match progress {
                            Ok(PtyEofProgress::Pending) => eof_retry_pending = true,
                            Ok(PtyEofProgress::Signaled) => {
                                let _ = done.send(StdinPumpResult::Eof(Ok(true)));
                            }
                            Ok(PtyEofProgress::Unsupported) => {
                                let _ = done.send(StdinPumpResult::Eof(Ok(false)));
                            }
                            Err(error) => {
                                let _ = done.send(StdinPumpResult::Eof(Err(error.to_string())));
                            }
                        }
                        eof_pending = eof_retry_pending;
                    }
                    if eof_retry_pending {
                        // Retrying immediately would monopolize the worker
                        // while a later terminal reply waits. Yield outside the
                        // publication gate, then re-check the priority channel.
                        std::thread::sleep(Duration::from_millis(1));
                        continue;
                    }
                }

                let write_result = if let Some(reply) = reply_current.as_ref() {
                    Some(pty_stdin.try_write(reply.remaining()))
                } else {
                    stdin_current
                        .as_ref()
                        .map(|stdin| pty_stdin.try_write(stdin.remaining()))
                };
                if let Some(result) = write_result {
                    match result {
                        Ok(0) => {
                            // Unix PTY capacity is temporarily exhausted.
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        Ok(written) if reply_current.is_some() => {
                            if reply_current
                                .as_mut()
                                .is_some_and(|reply| reply.advance(written))
                            {
                                reply_current = None;
                            }
                        }
                        Ok(written) => {
                            if stdin_current
                                .as_mut()
                                .is_some_and(|stdin| stdin.advance(written))
                            {
                                stdin_current = None;
                            }
                        }
                        Err(error) => {
                            let _ = done.send(StdinPumpResult::ForwardError(error.to_string()));
                            break;
                        }
                    }
                    continue;
                }

                if !replies_open && !stdin_open {
                    break;
                }
                let reply_receiver = if replies_open { &replies } else { &never_reply };
                let stdin_receiver = if stdin_open {
                    &stdin_events
                } else {
                    &never_stdin
                };
                crossbeam_channel::select_biased! {
                    recv(reply_receiver) -> message => match message {
                        Ok(bytes) => reply_current = PendingWrite::new(bytes),
                        Err(_) => replies_open = false,
                    },
                    recv(stdin_receiver) -> message => match message {
                        Ok(event) => process_stdin_event(
                            event,
                            &mut stdin_current,
                            &mut stdin_open,
                            &mut eof_pending,
                            &done,
                        ),
                        Err(_) => stdin_open = false,
                    },
                }
            }
        })
        .map(drop)
}

/// Read this process's stdin into bounded 8 KiB messages for the PTY writer
/// arbiter. On Windows, normalize LF/CRLF text to ConPTY's VT Enter (CR).
fn spawn_stdin_reader(events: Sender<StdinPumpEvent>) -> std::io::Result<()> {
    let task = Box::new(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 8192];
        #[cfg(windows)]
        let mut previous_was_cr = false;
        loop {
            match stdin.read(&mut buf) {
                Ok(0) => {
                    let _ = events.send(StdinPumpEvent::Eof);
                    break;
                }
                Ok(n) => {
                    #[cfg(windows)]
                    let bytes = {
                        let mut translated = Vec::with_capacity(n);
                        for &byte in &buf[..n] {
                            if byte == b'\n' {
                                if !previous_was_cr {
                                    translated.push(b'\r');
                                }
                            } else {
                                translated.push(byte);
                            }
                            previous_was_cr = byte == b'\r';
                        }
                        translated
                    };
                    #[cfg(not(windows))]
                    let bytes = buf[..n].to_vec();
                    if events.send(StdinPumpEvent::Data(bytes)).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = events.send(StdinPumpEvent::ReadError(error.to_string()));
                    break;
                }
            }
        }
    });
    spawn_stdin_pump_task_with(task, |task| {
        std::thread::Builder::new()
            .name("kettle-stdin-pump".into())
            .spawn(task)
            .map(drop)
    })
}

type StdinPumpTask = Box<dyn FnOnce() + Send + 'static>;

fn spawn_stdin_pump_task_with(
    task: StdinPumpTask,
    spawn: impl FnOnce(StdinPumpTask) -> std::io::Result<()>,
) -> std::io::Result<()> {
    spawn(task)
}

/// True when stdin is a pipe/file/socket we should forward to the PTY
/// (`echo y | kettle exec -- …`). On an interactive TTY we do NOT steal stdin
/// (the human is pointed at the GUI), and `/dev/null` stays closed rather than
/// being treated as useful input.
pub fn stdin_is_pipe() -> bool {
    #[cfg(unix)]
    {
        unsafe {
            let mut st: libc::stat = std::mem::zeroed();
            if libc::fstat(0, &mut st) != 0 {
                return false;
            }
            let kind = st.st_mode & libc::S_IFMT;
            kind == libc::S_IFIFO || kind == libc::S_IFREG || kind == libc::S_IFSOCK
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::INVALID_HANDLE_VALUE;
        use windows_sys::Win32::Storage::FileSystem::GetFileType;
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE};

        const FILE_TYPE_DISK: u32 = 0x0001;
        const FILE_TYPE_PIPE: u32 = 0x0003;
        let handle = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        if handle.is_null() || handle == INVALID_HANDLE_VALUE {
            return false;
        }
        matches!(
            unsafe { GetFileType(handle) },
            FILE_TYPE_DISK | FILE_TYPE_PIPE
        )
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

/// Best-effort console size as (cols, rows).
fn terminal_size_cols_rows() -> Option<(u16, u16)> {
    #[cfg(unix)]
    {
        // SAFETY: ioctl with a zeroed winsize on stdout; failure → None.
        unsafe {
            let mut ws: libc::winsize = std::mem::zeroed();
            if libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 {
                return Some((ws.ws_col, ws.ws_row.max(1)));
            }
        }
        None
    }
    #[cfg(windows)]
    {
        windows_console_size()
    }
    #[cfg(not(any(unix, windows)))]
    {
        None
    }
}

#[cfg(windows)]
fn windows_console_size() -> Option<(u16, u16)> {
    use windows_sys::Win32::System::Console::{
        CONSOLE_SCREEN_BUFFER_INFO, GetConsoleScreenBufferInfo, GetStdHandle, STD_OUTPUT_HANDLE,
    };
    unsafe {
        let h = GetStdHandle(STD_OUTPUT_HANDLE);
        if h.is_null() {
            return None;
        }
        let mut info: CONSOLE_SCREEN_BUFFER_INFO = std::mem::zeroed();
        if GetConsoleScreenBufferInfo(h, &mut info) != 0 {
            let cols = (info.srWindow.Right - info.srWindow.Left + 1).max(1) as u16;
            let rows = (info.srWindow.Bottom - info.srWindow.Top + 1).max(1) as u16;
            return Some((cols, rows));
        }
        None
    }
}

#[cfg(test)]
mod tests {

    fn opts_for(argv: Vec<String>, cwd: Option<PathBuf>) -> ExecOpts {
        ExecOpts {
            argv,
            cols: 80,
            rows: 24,
            cwd,
            timeout: None,
            mode: OutputMode::Raw,
            record: None,
            forward_stdin: false,
        }
    }

    /// A capturing caller must be told *why* a run never started.
    ///
    /// `run_exec_capture` returns an exit code and the sink, and the MCP
    /// `kettle_run` tool builds its whole reply from those two. Before this,
    /// a command that could not be spawned produced `exit code: 125` and an
    /// empty capture, so an agent that mistyped a binary name had nothing to
    /// act on and 125 is documented as an internal error.
    #[test]
    fn a_capturing_caller_is_told_why_the_command_never_started() {
        let missing = opts_for(vec!["kettle-no-such-binary-8f21c".into()], None);
        let (code, output) = run_exec_capture(missing);
        assert_eq!(code, EXIT_INTERNAL);
        assert!(
            output.contains("cannot start PTY"),
            "the capture must carry the reason, got {output:?}"
        );

        let bad_cwd = opts_for(
            vec!["echo".into()],
            Some("/kettle-no-such-directory-8f21c".into()),
        );
        let (code, output) = run_exec_capture(bad_cwd);
        assert_eq!(code, EXIT_INTERNAL);
        assert!(
            output.contains("invalid --cwd"),
            "a bad working directory must say so, got {output:?}"
        );
    }

    /// The terminal already shows stderr, so stdout must not repeat it.
    #[test]
    fn an_interactive_run_does_not_print_the_reason_twice() {
        let mut sink: Vec<u8> = Vec::new();
        let opts = opts_for(vec!["kettle-no-such-binary-8f21c".into()], None);
        let code = run_exec_with(opts, &|| None, &mut sink);
        assert_eq!(code, EXIT_INTERNAL);
        assert!(
            sink.is_empty(),
            "stdout should stay clean for a caller that can see stderr, got {:?}",
            String::from_utf8_lossy(&sink)
        );
    }
    use super::*;

    /// `--strip-ansi` corrupted ordinary text, and MCP `kettle_run` strips by
    /// default — so this was the output an agent CLI read.
    ///
    /// The C1 controls the stripper recognizes (`0x90` DCS, `0x98` SOS, `0x9b`
    /// CSI, `0x9d` OSC, `0x9e` PM, `0x9f` APC) all sit in UTF-8's
    /// `0x80..=0xbf` continuation range. Honoring them anywhere ate the middle
    /// of characters and emitted invalid UTF-8: `Ûh` became a lone `c3`.
    ///
    /// The three cases below are not exotic. Box-drawing and smart quotes are
    /// what a TUI — tmux, AstroNvim, any agent's status output — emits
    /// constantly.
    #[test]
    fn stripping_ansi_does_not_eat_the_middle_of_a_utf8_character() {
        for (label, text) in [
            ("U+00db, whose second byte is 0x9b CSI", "Ûh"),
            ("U+2018, whose third byte is 0x98 SOS", "\u{2018}x"),
            ("U+2590, whose third byte is 0x90 DCS", "\u{2590}y"),
            ("U+009d as OSC vs U+00dd", "Ýz"),
            ("astral plane, four bytes", "\u{1f980}!"),
            ("mixed run", "┌─┐ ‘quoted’ Ünicode └─┘"),
        ] {
            let mut stripper = AnsiStripper::default();
            let mut out = Vec::new();
            stripper.push(text.as_bytes(), &mut out);
            assert_eq!(
                String::from_utf8(out.clone()).as_deref(),
                Ok(text),
                "{label}: plain text must survive stripping byte for byte, got {out:02x?}"
            );
        }
    }

    /// A UTF-8 character INSIDE a control string must not terminate it.
    ///
    /// Tracking continuation bytes only in ground state left `0x9c` inside an
    /// OSC reading as ST: `ESC ] 0 ; ✳ title BEL X` (`✳` is `e2 9c b3`) ended
    /// the string at the `9c` and leaked `b3 title BEL X` into the output as
    /// visible garbage. Titles are exactly where non-ASCII shows up.
    #[test]
    fn a_utf8_character_inside_a_control_string_does_not_terminate_it() {
        for (label, payload) in [
            ("U+2733 contains 0x9c, which is ST", "\u{2733}"),
            ("U+2590 contains 0x90, which is DCS", "\u{2590}"),
            ("U+2018 contains 0x98, which is SOS", "\u{2018}"),
            ("U+00db contains 0x9b, which is CSI", "\u{00db}"),
        ] {
            let mut input = b"\x1b]0;".to_vec();
            input.extend_from_slice(payload.as_bytes());
            input.extend_from_slice(b" title\x07visible");

            let mut stripper = AnsiStripper::default();
            let mut out = Vec::new();
            stripper.push(&input, &mut out);
            assert_eq!(
                String::from_utf8_lossy(&out),
                "visible",
                "{label}: the whole OSC must be stripped, with nothing leaking"
            );
        }
    }

    /// A malformed lead byte must not shield the bytes after it.
    ///
    /// Counting N continuations unconditionally swallowed whatever followed,
    /// so a real control could be missed and a lead-followed-by-lead
    /// desynchronized the parser. A byte that is not `0x80..=0xbf` ends the
    /// shield immediately and is interpreted on its own terms.
    #[test]
    fn a_malformed_lead_byte_does_not_swallow_what_follows() {
        // `e2` promises two continuations; `9b` is one, but `31` is not — so
        // the sequence is malformed and `31` onward must be read normally.
        let mut stripper = AnsiStripper::default();
        let mut out = Vec::new();
        stripper.push(&[0xe2, 0x9b, 0x31, 0x6d, b'X'], &mut out);
        assert!(
            out.ends_with(b"X"),
            "text after a malformed sequence must still be emitted, got {out:02x?}"
        );

        // A lead immediately followed by another lead: the first is abandoned,
        // the second starts a real character.
        let mut stripper = AnsiStripper::default();
        let mut out = Vec::new();
        let mut input = vec![0xe2];
        input.extend_from_slice("é!".as_bytes());
        stripper.push(&input, &mut out);
        assert!(
            String::from_utf8_lossy(&out).ends_with("é!"),
            "the second character must survive, got {out:02x?}"
        );

        // A real 8-bit CSI after a broken lead is still recognised and removed.
        let mut stripper = AnsiStripper::default();
        let mut out = Vec::new();
        stripper.push(&[0xe2, 0x41, 0x9b, b'3', b'1', b'm', b'Z'], &mut out);
        assert!(
            out.ends_with(b"Z") && !out.contains(&b'm'),
            "the CSI must still be stripped after a malformed lead, got {out:02x?}"
        );
    }

    /// A character split across two chunks must still survive — the PTY hands
    /// output over in arbitrary slices, so a lead byte and its continuations
    /// routinely arrive separately.
    #[test]
    fn a_utf8_character_split_across_chunks_survives_stripping() {
        let text = "Û▐‘";
        let bytes = text.as_bytes();
        for split in 1..bytes.len() {
            let mut stripper = AnsiStripper::default();
            let mut out = Vec::new();
            stripper.push(&bytes[..split], &mut out);
            stripper.push(&bytes[split..], &mut out);
            assert_eq!(
                String::from_utf8(out.clone()).as_deref(),
                Ok(text),
                "split at {split} must not corrupt the character straddling it"
            );
        }
    }

    /// The escape sequences it exists to remove must still go — including the
    /// 8-bit C1 forms in ground position, where UTF-8 cannot place them.
    #[test]
    fn stripping_ansi_still_removes_the_sequences_it_is_for() {
        for (label, input, want) in [
            ("CSI SGR", b"\x1b[31mred\x1b[0m".to_vec(), "red"),
            ("OSC title", b"\x1b]0;title\x07after".to_vec(), "after"),
            (
                "8-bit CSI in ground",
                vec![0x9b, b'3', b'1', b'm', b'x'],
                "x",
            ),
            ("8-bit OSC in ground", vec![0x9d, b't', 0x9c, b'y'], "y"),
        ] {
            let mut stripper = AnsiStripper::default();
            let mut out = Vec::new();
            stripper.push(&input, &mut out);
            assert_eq!(
                String::from_utf8_lossy(&out),
                want,
                "{label}: the sequence must still be stripped"
            );
        }
    }

    #[test]
    fn output_slice_bounds_a_continuously_refilled_channel() {
        let (sender, receiver) = crossbeam_channel::unbounded();
        for _ in 0..=OUTPUT_SLICE_MESSAGES {
            sender.send(vec![b'x']).unwrap();
        }
        let mut recorder = None;
        let mut sink = Vec::new();
        let mut output = DirectOutput::new(OutputMode::Raw, &mut sink);
        let eof = std::cell::Cell::new(false);
        let last = std::cell::Cell::new(Instant::now());

        assert!(drain_output_slice(&receiver, &mut recorder, &mut output, &eof, &last).unwrap());
        assert_eq!(receiver.len(), 1);
        assert!(!drain_output_slice(&receiver, &mut recorder, &mut output, &eof, &last).unwrap());
        drop(output);
        assert_eq!(sink.len(), OUTPUT_SLICE_MESSAGES + 1);
    }

    /// An empty channel and a finished PTY are different facts, and the
    /// lifecycle loop used to act on the first while meaning the second.
    ///
    /// The reader thread owns the only sender, so "disconnected" proves it has
    /// ended; the separately published reader status distinguishes orderly EOF
    /// from an unexpected failure. "Empty" proves neither and is equally
    /// consistent with the reader simply not having run yet — routine on a
    /// loaded machine. Conflating them lost the child's output: for a command
    /// that writes a little and exits at once, the exit could be seen and the
    /// settle window elapse while the bytes were still in flight.
    ///
    /// Two macOS intermittents that looked unrelated were this one bug, because
    /// a single gate feeds both stdout and the recorder:
    /// `exec_streams_stdout_and_exits_zero` seeing empty stdout, and
    /// `exec_record_writes_replayable_asciicast` writing a header-only trace.
    #[test]
    fn a_quiet_channel_is_not_a_finished_pty() {
        let mut recorder = None;
        let mut sink = Vec::new();
        let mut output = DirectOutput::new(OutputMode::Raw, &mut sink);

        // Sender alive, nothing queued yet: this is the reader that has not
        // been scheduled. It must NOT read as end-of-output.
        let (sender, receiver) = crossbeam_channel::unbounded::<Vec<u8>>();
        let eof = std::cell::Cell::new(false);
        let last = std::cell::Cell::new(Instant::now());
        assert!(!drain_output_slice(&receiver, &mut recorder, &mut output, &eof, &last).unwrap());
        assert!(
            !eof.get(),
            "an empty channel with a live reader must not latch EOF -- that is \
             precisely the moment the output is still in flight"
        );

        // The bytes arrive late, exactly as they did in the race.
        sender.send(b"recmark-9z".to_vec()).unwrap();
        assert!(!drain_output_slice(&receiver, &mut recorder, &mut output, &eof, &last).unwrap());
        assert!(
            !eof.get(),
            "delivering output is not EOF either; the reader may have more"
        );

        // Now the reader exits and drops its sender. THAT is end-of-output.
        drop(sender);
        assert!(!drain_output_slice(&receiver, &mut recorder, &mut output, &eof, &last).unwrap());
        assert!(
            eof.get(),
            "a disconnected channel proves the reader ended; its status still \
             decides whether that end was EOF or failure"
        );

        drop(output);
        assert_eq!(
            sink, b"recmark-9z",
            "and the late bytes still reached stdout rather than being dropped \
             on the way to the conclusion"
        );
    }

    /// The old lifecycle used the ConPTY fallback on every platform. At 810ms
    /// a loaded Unix reader that had not run yet therefore looked complete,
    /// even though its sole sender was still alive and the child's bytes were
    /// in flight. That produced exit 0 with empty stdout and header-only casts.
    #[test]
    fn unix_elapsed_time_never_turns_missing_eof_into_success() {
        let old_cross_platform_fallback = SETTLE + CONPTY_DRAIN_GRACE;
        assert_eq!(
            pty_drain_decision(
                false,
                PtyTransportEnd::Pending,
                old_cross_platform_fallback,
                old_cross_platform_fallback,
                true,
                0,
            ),
            PtyDrainDecision::Wait,
            "the former 810ms fallback must not claim Unix output is complete"
        );
        assert_eq!(
            pty_drain_decision(
                false,
                PtyTransportEnd::Pending,
                PTY_EOF_TIMEOUT,
                PTY_EOF_TIMEOUT,
                true,
                0,
            ),
            PtyDrainDecision::Failed(PtyDrainFailure::EofTimeout),
            "a genuinely stuck Unix reader must fail closed, not hang forever"
        );
        assert_eq!(
            pty_drain_decision(
                false,
                PtyTransportEnd::Eof,
                Duration::ZERO,
                Duration::ZERO,
                true,
                0,
            ),
            PtyDrainDecision::Finished,
            "an orderly EOF remains immediate completion proof"
        );
        assert_eq!(
            pty_drain_decision(
                false,
                PtyTransportEnd::Eof,
                Duration::ZERO,
                Duration::ZERO,
                true,
                1,
            ),
            PtyDrainDecision::Failed(PtyDrainFailure::Read),
            "a disconnected reader cannot retire parser work it stranded"
        );
    }

    #[test]
    fn unix_eof_timeout_does_not_override_downstream_backpressure() {
        assert_eq!(
            pty_drain_decision(
                false,
                PtyTransportEnd::Pending,
                PTY_EOF_TIMEOUT,
                PTY_EOF_TIMEOUT,
                false,
                0,
            ),
            PtyDrainDecision::Wait,
            "a queued raw chunk or busy stdout worker is delivery work, not a stuck PTY reader"
        );
    }

    #[test]
    fn an_accepted_stdout_command_is_not_idle_until_its_write_returns() {
        struct GateWriter {
            entered: Sender<()>,
            release: Receiver<()>,
        }

        impl Write for GateWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                let _ = self.entered.try_send(());
                self.release
                    .recv_timeout(Duration::from_secs(1))
                    .expect("test releases the blocked stdout write");
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
        let (release_tx, release_rx) = crossbeam_channel::bounded(1);
        let mut output = WorkerOutput::spawn(
            OutputMode::Raw,
            GateWriter {
                entered: entered_tx,
                release: release_rx,
            },
        )
        .unwrap();
        output.output(vec![b'x']).unwrap();
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("stdout worker entered the OS-facing write");

        assert!(
            !output.drained().unwrap(),
            "an empty command queue does not make its in-flight write idle"
        );
        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !output.drained().unwrap() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(output.drained().unwrap());
    }

    #[test]
    fn stranded_reader_state_waits_for_an_admitted_stdout_write() {
        struct GateWriter {
            entered: Sender<()>,
            release: Receiver<()>,
        }

        impl Write for GateWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                let _ = self.entered.try_send(());
                self.release
                    .recv_timeout(Duration::from_secs(1))
                    .expect("test releases the blocked stdout write");
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
        let (release_tx, release_rx) = crossbeam_channel::bounded(1);
        let mut output = WorkerOutput::spawn(
            OutputMode::Raw,
            GateWriter {
                entered: entered_tx,
                release: release_rx,
            },
        )
        .unwrap();
        output.output(vec![b'x']).unwrap();
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("stdout worker entered the OS-facing write");

        for transport_end in [PtyTransportEnd::Eof, PtyTransportEnd::Failed] {
            assert_eq!(
                pty_drain_decision(
                    cfg!(windows),
                    transport_end,
                    Duration::from_secs(60),
                    Duration::from_secs(60),
                    output.drained().unwrap(),
                    1,
                ),
                PtyDrainDecision::Wait,
                "{transport_end:?} with a stranded source count must not abandon an admitted write"
            );
        }

        release_tx.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while !output.drained().unwrap() && Instant::now() < deadline {
            std::thread::yield_now();
        }
        assert!(output.drained().unwrap());
        for transport_end in [PtyTransportEnd::Eof, PtyTransportEnd::Failed] {
            assert_eq!(
                pty_drain_decision(
                    cfg!(windows),
                    transport_end,
                    Duration::from_secs(60),
                    Duration::from_secs(60),
                    true,
                    1,
                ),
                PtyDrainDecision::Failed(PtyDrainFailure::Read),
                "{transport_end:?} must remain a read failure once admitted output drains"
            );
        }
    }

    #[test]
    fn a_failed_stdout_write_is_published_before_its_drain_acknowledgement() {
        let outstanding = AtomicUsize::new(1);
        let mut published = false;
        assert!(retire_output_command(
            &outstanding,
            Err(std::io::Error::other("synthetic stdout failure")),
            false,
            |outcome| {
                assert_eq!(
                    outstanding.load(Ordering::Acquire),
                    1,
                    "the worker must publish its error before it can look drained"
                );
                assert_eq!(
                    outcome,
                    Err(OutputDeliveryError::unexpected("synthetic stdout failure"))
                );
                published = true;
            },
        ));
        assert!(published);
        assert_eq!(outstanding.load(Ordering::Acquire), 0);

        struct FailingWriter;

        impl Write for FailingWriter {
            fn write(&mut self, _bytes: &[u8]) -> std::io::Result<usize> {
                Err(std::io::Error::other("synthetic stdout failure"))
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let mut output = WorkerOutput::spawn(OutputMode::Raw, FailingWriter).unwrap();
        output.output(vec![b'x']).unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            match output.drained() {
                Err(error) => {
                    assert_eq!(
                        error,
                        OutputDeliveryError::unexpected("synthetic stdout failure")
                    );
                    break;
                }
                Ok(false) if Instant::now() < deadline => std::thread::yield_now(),
                Ok(false) => panic!("stdout failure was not published within the test deadline"),
                Ok(true) => panic!("a failed stdout write was reported as successfully drained"),
            }
        }
    }

    #[test]
    fn final_completion_requires_a_stable_eof_snapshot() {
        let quiet_snapshot = Some(7);
        assert!(pty_completion_snapshot_is_current(
            quiet_snapshot,
            PtyReadProgress {
                status: PtyReadStatus::Eof,
                generation: 7,
                pending_chunks: 0,
            }
        ));
        assert!(!pty_completion_snapshot_is_current(
            quiet_snapshot,
            PtyReadProgress {
                status: PtyReadStatus::Reading,
                generation: 7,
                pending_chunks: 0,
            }
        ));
        assert!(!pty_completion_snapshot_is_current(
            quiet_snapshot,
            PtyReadProgress {
                status: PtyReadStatus::Reading,
                generation: 8,
                pending_chunks: 1,
            }
        ));
        assert!(!pty_completion_snapshot_is_current(
            quiet_snapshot,
            PtyReadProgress {
                status: PtyReadStatus::Reading,
                generation: 7,
                pending_chunks: 1,
            }
        ));
    }

    #[test]
    fn conpty_quiet_only_starts_an_authoritative_transport_close() {
        let boundary = SETTLE + CONPTY_DRAIN_GRACE;
        let base = Instant::now();
        assert_eq!(
            latest_pty_activity(
                base,
                base + Duration::from_millis(1),
                base + Duration::from_millis(2),
            ),
            base + Duration::from_millis(2),
            "a pump read restarts quiet time before lifecycle delivery"
        );
        assert_eq!(
            pty_drain_decision(true, PtyTransportEnd::Pending, boundary, boundary, true, 0,),
            PtyDrainDecision::CloseTransport,
            "quiet permits closing ConPTY, not completing while it can still emit"
        );
        assert_eq!(
            pty_drain_decision(
                true,
                PtyTransportEnd::Pending,
                boundary + Duration::from_millis(1),
                Duration::ZERO,
                true,
                0,
            ),
            PtyDrainDecision::Wait,
            "a late repaint restarts the quiet window instead of inheriting the child-exit timer"
        );
        assert_eq!(
            pty_drain_decision(
                true,
                PtyTransportEnd::Pending,
                boundary * 2,
                boundary,
                false,
                0,
            ),
            PtyDrainDecision::Wait,
            "queued output or a busy writer is not quiet completion"
        );
        assert_eq!(
            pty_drain_decision(
                true,
                PtyTransportEnd::Pending,
                boundary * 2,
                boundary,
                true,
                1,
            ),
            PtyDrainDecision::Wait,
            "a source-read chunk hidden in the parser pipeline is not quiet completion"
        );
    }

    #[test]
    fn an_unexpected_reader_end_is_never_success() {
        assert_eq!(
            observed_pty_transport_end(false, PtyReadStatus::Failed),
            PtyTransportEnd::Pending,
            "failure is sticky, but chunks before its marker still have to drain"
        );
        assert_eq!(
            observed_pty_transport_end(true, PtyReadStatus::Failed),
            PtyTransportEnd::Failed
        );
        assert_eq!(
            pty_drain_decision(
                cfg!(windows),
                PtyTransportEnd::Failed,
                Duration::from_secs(60),
                Duration::from_secs(60),
                false,
                0,
            ),
            PtyDrainDecision::Wait,
            "an occupied stdout writer must finish already admitted bytes before failure"
        );
        assert_eq!(
            pty_drain_decision(
                cfg!(windows),
                PtyTransportEnd::Failed,
                Duration::from_secs(60),
                Duration::from_secs(60),
                true,
                2,
            ),
            PtyDrainDecision::Failed(PtyDrainFailure::Read),
            "a disconnected parser cannot retire its remaining pending count"
        );
        assert_eq!(
            pty_drain_decision(
                cfg!(windows),
                PtyTransportEnd::Failed,
                Duration::from_secs(60),
                Duration::from_secs(60),
                true,
                0,
            ),
            PtyDrainDecision::Failed(PtyDrainFailure::Read)
        );
    }

    #[test]
    fn a_core_eof_timeout_remains_an_eof_timeout_after_output_drains() {
        assert_eq!(
            observed_pty_transport_end(false, PtyReadStatus::EofTimeout),
            PtyTransportEnd::Pending,
            "chunks ordered ahead of the timeout marker must still drain"
        );
        assert_eq!(
            observed_pty_transport_end(true, PtyReadStatus::EofTimeout),
            PtyTransportEnd::EofTimeout
        );
        assert_eq!(
            pty_drain_decision(
                false,
                PtyTransportEnd::EofTimeout,
                Duration::from_secs(5),
                Duration::from_secs(5),
                true,
                0,
            ),
            PtyDrainDecision::Failed(PtyDrainFailure::EofTimeout)
        );
    }

    #[cfg(not(windows))]
    #[test]
    fn native_unix_selection_rejects_the_old_conpty_fallback() {
        let old_boundary = SETTLE + CONPTY_DRAIN_GRACE;
        assert_eq!(
            native_pty_drain_decision(
                PtyTransportEnd::Pending,
                old_boundary,
                old_boundary,
                true,
                0,
            ),
            PtyDrainDecision::Wait,
            "the production platform seam must select Unix EOF semantics"
        );
    }

    #[cfg(windows)]
    #[test]
    fn native_windows_selection_retains_the_conpty_fallback() {
        let boundary = SETTLE + CONPTY_DRAIN_GRACE;
        assert_eq!(
            native_pty_drain_decision(PtyTransportEnd::Pending, boundary, boundary, true, 0,),
            PtyDrainDecision::CloseTransport,
            "the production platform seam must close ConPTY before completion"
        );
    }

    #[test]
    fn a_deadline_cannot_return_a_known_child_success_with_live_output() {
        assert_eq!(
            lifecycle_stop_code(LifecycleStop::Deadline, Some(0)),
            EXIT_TIMEOUT,
            "a successful child status does not complete the lossless PTY delivery contract"
        );
    }

    #[test]
    fn output_slice_applies_an_independent_byte_budget() {
        let (sender, receiver) = crossbeam_channel::unbounded();
        let chunk_len = OUTPUT_SLICE_BYTES / 2 + 1;
        for _ in 0..3 {
            sender.send(vec![b'x'; chunk_len]).unwrap();
        }
        let mut recorder = None;
        let mut sink = Vec::new();
        let mut output = DirectOutput::new(OutputMode::Raw, &mut sink);
        let eof = std::cell::Cell::new(false);
        let last = std::cell::Cell::new(Instant::now());

        assert!(drain_output_slice(&receiver, &mut recorder, &mut output, &eof, &last).unwrap());
        drop(output);
        assert_eq!(sink.len(), chunk_len * 2);
        assert_eq!(receiver.len(), 1);
    }

    #[test]
    fn semantic_event_slice_bounds_a_continuously_refilled_channel() {
        let (sender, receiver) = crossbeam_channel::unbounded();
        for value in 0..=EVENT_SLICE_MESSAGES {
            sender.send(value).unwrap();
        }
        let mut handled = Vec::new();

        assert!(drain_event_slice(&receiver, |event| handled.push(event)));
        assert_eq!(handled.len(), EVENT_SLICE_MESSAGES);
        assert!(!drain_event_slice(&receiver, |event| handled.push(event)));
        assert_eq!(handled.len(), EVENT_SLICE_MESSAGES + 1);
    }

    #[cfg(any(unix, windows))]
    #[derive(Default)]
    struct StopBeforeReadinessOutput {
        finished: Option<i32>,
    }

    #[cfg(any(unix, windows))]
    impl ExecOutput for StopBeforeReadinessOutput {
        fn ready(&mut self) -> OutputResult<bool> {
            panic!("output readiness ran before an imposed lifecycle stop");
        }

        fn start(&mut self, _cols: u16, _rows: u16) -> OutputResult<()> {
            Ok(())
        }

        fn output(&mut self, _bytes: Vec<u8>) -> OutputResult<()> {
            panic!("output was emitted before an imposed lifecycle stop");
        }

        fn title(&mut self, _title: String) -> OutputResult<()> {
            panic!("a title was emitted before an imposed lifecycle stop");
        }

        fn finish(
            &mut self,
            code: i32,
            _duration: Duration,
            mode: OutputFinish,
        ) -> OutputResult<()> {
            assert!(matches!(mode, OutputFinish::AbandonPending));
            self.finished = Some(code);
            Ok(())
        }
    }

    #[cfg(any(unix, windows))]
    fn long_running_stop_test_opts(timeout: Option<Duration>) -> ExecOpts {
        #[cfg(unix)]
        let argv = vec!["sh".into(), "-c".into(), "sleep 30".into()];
        #[cfg(windows)]
        let argv = vec![
            "cmd.exe".into(),
            "/D".into(),
            "/S".into(),
            "/C".into(),
            "ping -n 30 127.0.0.1 >NUL".into(),
        ];
        ExecOpts {
            argv,
            cols: 80,
            rows: 24,
            cwd: None,
            timeout,
            mode: OutputMode::Raw,
            record: None,
            forward_stdin: false,
        }
    }

    #[cfg(any(unix, windows))]
    fn assert_stop_precedes_output_readiness(
        opts: ExecOpts,
        cancelled: Option<&AtomicBool>,
        expected: i32,
    ) {
        let mut output = StopBeforeReadinessOutput::default();
        let code = run_exec_engine(opts, &|| None, &mut output, cancelled, true);
        if code == EXIT_INTERNAL && output.finished.is_none() {
            eprintln!("skipping lifecycle stop ordering test: no PTY");
            return;
        }
        // The imposed stop wins before `ready` either way. Teardown is a
        // separate, deliberately fail-closed contract: Linux may return 125
        // when its bounded pidfd/procfs proof cannot authenticate the complete
        // process scope under scheduler pressure, and claiming 124/130 in that
        // case would be the product bug. Pin both truthful outcomes and require
        // `finish` to publish the exact one the lifecycle chose.
        assert!(
            code == expected || code == EXIT_INTERNAL,
            "an imposed lifecycle stop returned unexpected status {code}"
        );
        assert_eq!(output.finished, Some(code));
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn timeout_is_evaluated_before_output_readiness() {
        assert_stop_precedes_output_readiness(
            long_running_stop_test_opts(Some(Duration::ZERO)),
            None,
            EXIT_TIMEOUT,
        );
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn cancellation_is_evaluated_before_output_readiness() {
        let cancelled = AtomicBool::new(true);
        assert_stop_precedes_output_readiness(
            long_running_stop_test_opts(None),
            Some(&cancelled),
            EXIT_CANCELLED,
        );
    }

    #[test]
    fn empty_stdin_frames_never_enter_the_writer_state_machine() {
        let (done_tx, done_rx) = crossbeam_channel::bounded(1);
        let mut current = None;
        let mut stdin_open = true;
        let mut eof_pending = false;

        process_stdin_event(
            StdinPumpEvent::Data(Vec::new()),
            &mut current,
            &mut stdin_open,
            &mut eof_pending,
            &done_tx,
        );

        assert!(current.is_none());
        assert!(stdin_open);
        assert!(!eof_pending);
        assert!(done_rx.is_empty());
    }

    #[test]
    fn ansi_stripper_handles_sequence_split_across_chunks() {
        // A CSI color sequence split mid-way across two pushes must be fully
        // stripped, with the plain text on both sides preserved.
        let mut s = AnsiStripper::default();
        let mut out = Vec::new();
        s.push(b"a\x1b[3", &mut out); // ESC [ 3 — incomplete, held
        assert_eq!(out, b"a", "text before the escape flushes; escape held");
        s.push(b"1mb", &mut out); // …1 m  → terminator, then 'b'
        assert_eq!(out, b"ab", "completed escape stripped, trailing text kept");
    }

    #[test]
    fn ansi_stripper_osc_and_single_char() {
        let mut s = AnsiStripper::default();
        let mut out = Vec::new();
        // OSC title ending in BEL, then text.
        s.push(b"\x1b]0;hi\x07X", &mut out);
        assert_eq!(out, b"X");
        // OSC ending in ST (ESC \).
        let mut s = AnsiStripper::default();
        let mut out = Vec::new();
        s.push(b"\x1b]8;;http://x\x1b\\Y", &mut out);
        assert_eq!(out, b"Y");
        // Single-char ESC (ESC c full reset).
        let mut s = AnsiStripper::default();
        let mut out = Vec::new();
        s.push(b"\x1bcZ", &mut out);
        assert_eq!(out, b"Z");
    }

    #[test]
    fn ansi_stripper_removes_string_control_payloads_and_c1_forms() {
        let mut stripper = AnsiStripper::default();
        let mut out = Vec::new();
        stripper.push(
            b"a\x1bP1;2|dcs\x1b\\b\x1b_apc\x1b\\c\x1b^pm\x1b\\d\x1bXsos\x1b\\e",
            &mut out,
        );
        assert_eq!(out, b"abcde");

        let mut stripper = AnsiStripper::default();
        let mut out = Vec::new();
        stripper.push(b"a\x90dcs\x9cb\x9dapc\x07c\x9b31md", &mut out);
        assert_eq!(out, b"abcd");
    }

    #[test]
    fn ansi_stripper_cancellation_and_nested_escape_recover_control_sequences() {
        for (label, input, expected) in [
            ("CAN cancels CSI", &b"\x1b[31\x18hello"[..], &b"hello"[..]),
            ("SUB cancels CSI", &b"\x1b[31\x1ahello"[..], &b"hello"[..]),
            (
                "CAN cancels escape intermediate",
                &b"\x1b(\x18hello"[..],
                &b"hello"[..],
            ),
            (
                "SUB cancels escape intermediate",
                &b"\x1b(\x1ahello"[..],
                &b"hello"[..],
            ),
            (
                "CAN cancels DCS",
                &b"\x1bPpayload\x18hello"[..],
                &b"hello"[..],
            ),
            (
                "SUB cancels SOS",
                &b"\x1bXpayload\x1ahello"[..],
                &b"hello"[..],
            ),
            (
                "CAN cancels PM",
                &b"\x1b^payload\x18hello"[..],
                &b"hello"[..],
            ),
            (
                "SUB cancels APC",
                &b"\x1b_payload\x1ahello"[..],
                &b"hello"[..],
            ),
            (
                "CAN cancels after a pending string ESC",
                &b"\x1b^payload\x1b\x18hello"[..],
                &b"hello"[..],
            ),
            (
                "single-character ESC aborts a control string",
                &b"\x1b^payload\x1bchello"[..],
                &b"hello"[..],
            ),
            (
                "single-character ESC aborts OSC too",
                &b"\x1b]0;title\x1bchello"[..],
                &b"hello"[..],
            ),
            (
                "nested CSI replaces a control string",
                &b"\x1b^payload\x1b[31mhello"[..],
                &b"hello"[..],
            ),
            (
                "nested OSC replaces a control string",
                &b"\x1b^payload\x1b]title\x07hello"[..],
                &b"hello"[..],
            ),
        ] {
            for split in 0..=input.len() {
                let mut stripper = AnsiStripper::default();
                let mut out = Vec::new();
                stripper.push(&input[..split], &mut out);
                stripper.push(&input[split..], &mut out);
                assert_eq!(
                    out, expected,
                    "{label} split at byte {split} must preserve the visible suffix"
                );
            }
        }
    }

    /// Every parser-state sibling crossed with every cancellation/termination
    /// event. Some pairs deliberately do not terminate: BEL is data in a
    /// DCS/APC-style string, while BEL and raw ST are parameter/intermediate
    /// bytes in CSI and ESC-intermediate states. The expected-state matrix
    /// pins those distinctions instead of making the test name promise more
    /// than it proves.
    #[test]
    fn ansi_stripper_control_events_cover_every_state_cross_product() {
        #[derive(Clone, Copy)]
        enum StateCase {
            Ground,
            Escape,
            EscapeIntermediate,
            Csi,
            OscString,
            DcsApcString,
        }

        impl StateCase {
            fn name(self) -> &'static str {
                match self {
                    Self::Ground => "Ground",
                    Self::Escape => "Escape",
                    Self::EscapeIntermediate => "EscapeIntermediate",
                    Self::Csi => "Csi",
                    Self::OscString => "OSC-string",
                    Self::DcsApcString => "DCS/APC-string",
                }
            }

            fn state(self) -> StripState {
                match self {
                    Self::Ground => StripState::Ground,
                    Self::Escape => StripState::Escape,
                    Self::EscapeIntermediate => StripState::EscapeIntermediate {
                        remaining: MAX_CONTROL_SEQUENCE_BYTES,
                    },
                    Self::Csi => StripState::Csi {
                        remaining: MAX_CONTROL_SEQUENCE_BYTES,
                    },
                    Self::OscString => StripState::String {
                        bel_terminated: true,
                        escaped: false,
                        remaining: MAX_CONTROL_SEQUENCE_BYTES,
                    },
                    Self::DcsApcString => StripState::String {
                        bel_terminated: false,
                        escaped: false,
                        remaining: MAX_CONTROL_SEQUENCE_BYTES,
                    },
                }
            }
        }

        #[derive(Clone, Copy)]
        enum Event {
            Can,
            Sub,
            FreshEscapeDispatch,
            EscBackslash,
            Bel,
            RawSt,
        }

        impl Event {
            fn name(self) -> &'static str {
                match self {
                    Self::Can => "CAN",
                    Self::Sub => "SUB",
                    Self::FreshEscapeDispatch => "fresh ESC + dispatch",
                    Self::EscBackslash => "ESC \\",
                    Self::Bel => "BEL",
                    Self::RawSt => "raw ST 0x9c",
                }
            }

            fn bytes(self) -> &'static [u8] {
                match self {
                    Self::Can => b"\x18",
                    Self::Sub => b"\x1a",
                    Self::FreshEscapeDispatch => b"\x1bc",
                    Self::EscBackslash => b"\x1b\\",
                    Self::Bel => b"\x07",
                    Self::RawSt => b"\x9c",
                }
            }
        }

        fn remains_open(state: StateCase, event: Event) -> bool {
            matches!(
                (state, event),
                (
                    StateCase::EscapeIntermediate | StateCase::Csi,
                    Event::Bel | Event::RawSt
                ) | (StateCase::DcsApcString, Event::Bel)
            )
        }

        fn is_open(state: &StripState) -> bool {
            !matches!(state, StripState::Ground)
        }

        let states = [
            StateCase::Ground,
            StateCase::Escape,
            StateCase::EscapeIntermediate,
            StateCase::Csi,
            StateCase::OscString,
            StateCase::DcsApcString,
        ];
        let events = [
            Event::Can,
            Event::Sub,
            Event::FreshEscapeDispatch,
            Event::EscBackslash,
            Event::Bel,
            Event::RawSt,
        ];
        let mut covered = 0;
        for state in states {
            for event in events {
                let bytes = event.bytes();
                for split in 0..=bytes.len() {
                    let mut stripper = AnsiStripper {
                        state: state.state(),
                        ..AnsiStripper::default()
                    };
                    let mut out = Vec::new();
                    stripper.push(&bytes[..split], &mut out);
                    stripper.push(&bytes[split..], &mut out);

                    assert_eq!(
                        is_open(&stripper.state),
                        remains_open(state, event),
                        "{} x {} split at {split} reached {:?}",
                        state.name(),
                        event.name(),
                        stripper.state
                    );

                    // Finish the deliberately-open combinations, then prove
                    // the parser returns to visible output rather than merely
                    // reaching a superficially plausible enum variant.
                    if is_open(&stripper.state) {
                        stripper.push(b"\x18", &mut out);
                    }
                    stripper.push(b"VISIBLE", &mut out);
                    let control_prefix: &[u8] = match (state, event) {
                        (StateCase::Ground, Event::Can) => b"\x18",
                        (StateCase::Ground, Event::Sub) => b"\x1a",
                        (StateCase::Ground, Event::Bel) => b"\x07",
                        _ => b"",
                    };
                    let mut expected = control_prefix.to_vec();
                    expected.extend_from_slice(b"VISIBLE");
                    assert_eq!(
                        out,
                        expected,
                        "{} x {} split at {split} must preserve the visible suffix",
                        state.name(),
                        event.name()
                    );
                }
                covered += 1;
            }
        }
        assert_eq!(covered, 6 * 6, "the full state/event cross product changed");
    }

    #[test]
    fn ansi_stripper_has_constant_memory_and_bounded_resynchronization() {
        assert!(std::mem::size_of::<AnsiStripper>() <= 32);
        let mut stripper = AnsiStripper::default();
        let mut out = Vec::new();
        stripper.push(b"prefix\x1b]unterminated", &mut out);
        for _ in 0..=(MAX_CONTROL_SEQUENCE_BYTES / 1024) {
            stripper.push(&[b'x'; 1024], &mut out);
        }
        stripper.push(b"tail", &mut out);

        assert!(out.starts_with(b"prefix"));
        assert!(out.ends_with(b"tail"));
        assert!(
            out.len() <= 2048,
            "resynchronization retained too much data"
        );
    }

    /// Valid UTF-8 in must stay valid UTF-8 out, including across the forced
    /// resynchronization that ends an over-long control string.
    ///
    /// The stripper shields UTF-8 continuation bytes so a `0x9c` inside a
    /// character is not mistaken for the 8-bit ST. That shield outlived the
    /// string: when the resynchronization bound fell on a multi-byte lead, the
    /// lead was consumed as the string's last byte while the debt survived into
    /// ground state, so the continuation bytes were emitted with nothing in
    /// front of them. Anything decoding stdout saw invalid UTF-8 from there on.
    ///
    /// The boundary is swept because the payload length that lands a lead byte
    /// exactly on it depends on how the state machine counts.
    ///
    /// All THREE bounded states are swept. Fixing only the control-string one
    /// left the identical hole in `Csi` and `EscapeIntermediate`, which have the
    /// same 64-KiB bound — a review found both still emitting invalid UTF-8
    /// after the first fix shipped.
    #[test]
    fn ansi_stripper_never_emits_orphaned_utf8_continuations_at_the_resync_bound() {
        // Each opener enters a different bounded state, with a filler byte that
        // keeps it there: OSC payload, CSI parameters, ESC intermediates.
        for (label, opener, filler) in [
            ("control string", &b"\x1b]0;"[..], b'x'),
            ("csi parameters", &b"\x1b["[..], b'0'),
            ("escape intermediates", &b"\x1b "[..], b' '),
        ] {
            for offset in -4_isize..=4 {
                let fill = (MAX_CONTROL_SEQUENCE_BYTES as isize + offset) as usize;
                let mut input = Vec::with_capacity(fill + 16);
                input.extend_from_slice(opener);
                input.resize(input.len() + fill, filler);
                // Three-byte lead plus its continuations, one of which is 0x9c.
                input.extend_from_slice("\u{672b}".as_bytes());
                // The resynchronization point moves with `offset`, and it eats
                // whatever bytes it lands on. Pad past that window so the marker is
                // always in ground state by the time it arrives.
                input.extend_from_slice(&[b'z'; 32]);
                input.extend_from_slice(b"tail");

                let mut stripper = AnsiStripper::default();
                let mut out = Vec::new();
                stripper.push(&input, &mut out);
                let text = String::from_utf8(out.clone()).unwrap_or_else(|error| {
                    panic!(
                        "{label}, fill {fill}: stripped output is not valid UTF-8 \
                     ({error}); the last bytes were {:?}",
                        &out[out.len().saturating_sub(16)..]
                    )
                });
                assert!(
                    text.ends_with("tail"),
                    "{label}, fill {fill}: the stream must resynchronize and pass \
                 text through, got {text:?}"
                );
            }
        }

        // And with no 64 KiB involved at all: `ESC` followed directly by a lead
        // byte consumes that byte as a one-character escape, so its
        // continuations must be consumed with it rather than surfacing alone.
        let mut input = Vec::new();
        input.extend_from_slice(b"head\x1b");
        input.extend_from_slice("\u{672b}".as_bytes());
        input.extend_from_slice(b"tail");
        let mut stripper = AnsiStripper::default();
        let mut out = Vec::new();
        stripper.push(&input, &mut out);
        let text = String::from_utf8(out.clone()).unwrap_or_else(|error| {
            panic!(
                "ESC + lead byte: stripped output is not valid UTF-8 ({error}); \
                 bytes were {out:?}"
            )
        });
        assert_eq!(
            text, "headtail",
            "the escaped character is consumed whole, not half"
        );
    }

    /// The capture sink must keep the last `cap` bytes, and must not do
    /// quadratic work to keep them.
    ///
    /// It trimmed to exactly `cap` on every write, so once full a small chunk
    /// shifted the whole buffer down by that chunk's size. A build emitting 100
    /// MiB moved roughly 25 GiB of memory to retain the last 1 MiB — on the
    /// thread draining the PTY. Compaction is amortized now; the answer must be
    /// unchanged.
    #[test]
    fn the_capture_sink_keeps_the_tail_without_quadratic_shifting() {
        use std::io::Write as _;

        // Every write size around the cap, plus the exact boundaries.
        for cap in [1usize, 2, 7, 64] {
            for chunk in [1usize, 3, 7, 64, 65, 200] {
                let mut sink = TailSink::new(cap);
                let mut written: Vec<u8> = Vec::new();
                // Enough rounds to compact several times over.
                for round in 0..40u16 {
                    let data: Vec<u8> = (0..chunk).map(|i| (round as usize + i) as u8).collect();
                    assert_eq!(sink.write(&data).unwrap(), data.len());
                    written.extend_from_slice(&data);
                    assert_eq!(
                        sink.tail(),
                        &written[written.len() - cap.min(written.len())..],
                        "cap {cap}, chunk {chunk}, round {round}: wrong tail"
                    );
                    // Lazy compaction is allowed slack, but must stay bounded —
                    // that bound is the whole point of a tail sink.
                    assert!(
                        sink.buf.len() <= cap * 2 + chunk,
                        "cap {cap}, chunk {chunk}: buffer grew to {} bytes",
                        sink.buf.len()
                    );
                }
            }
        }

        // A single write larger than the cap keeps only its own tail, and does
        // not first grow the buffer to the size of that write.
        let mut sink = TailSink::new(8);
        assert_eq!(sink.write(&[b'x'; 4]).unwrap(), 4);
        assert_eq!(sink.write(b"0123456789abcdef").unwrap(), 16);
        assert_eq!(sink.tail(), b"89abcdef");
        assert_eq!(
            sink.buf.len(),
            8,
            "an oversized write must copy only what survives"
        );

        // And a zero-length write changes nothing.
        let before = sink.tail().to_vec();
        assert_eq!(sink.write(b"").unwrap(), 0);
        assert_eq!(sink.tail(), before.as_slice());
    }

    #[test]
    fn ansi_stripper_bare_trailing_escape_dropped() {
        let mut s = AnsiStripper::default();
        let mut out = Vec::new();
        s.push(b"hi\x1b", &mut out);
        assert_eq!(
            out, b"hi",
            "incomplete trailing escape is held, not emitted"
        );
    }

    #[test]
    fn clamp_code_per_platform() {
        // Small codes pass through on every platform.
        assert_eq!(clamp_code(3), 3);
        // Unix masks to the low 8 bits (process::exit + signal folding);
        // Windows passes the full 32-bit code through (saturating into i32).
        #[cfg(unix)]
        {
            assert_eq!(clamp_code(256), 0);
            assert_eq!(clamp_code(3221225786), (3221225786u32 & 0xff) as i32);
        }
        #[cfg(windows)]
        {
            assert_eq!(clamp_code(256), 256);
            // 0xC0000005 (STATUS_ACCESS_VIOLATION) must survive as itself. This
            // previously asserted `i32::MAX`, pinning the saturation that threw
            // the diagnostic away — the crash code an agent or CI script reads
            // to tell an access violation from any other failure.
            assert_eq!(clamp_code(0xC000_0005), 0xC000_0005u32 as i32);
            assert_eq!(clamp_code(0xC000_0005) as u32, 0xC000_0005);
            // Every crash-class NTSTATUS round-trips, not just that one.
            for status in [
                0xC000_0005u32,
                0xC000_001D,
                0xC000_0094,
                0xC000_0409,
                0x8000_0003,
            ] {
                assert_eq!(
                    clamp_code(status) as u32,
                    status,
                    "NTSTATUS {status:#010x} must reach the caller intact"
                );
            }
            assert_eq!(clamp_code(3), 3);
        }
    }

    #[test]
    fn empty_argv_is_internal_error() {
        let opts = ExecOpts {
            argv: vec![],
            cols: 80,
            rows: 24,
            cwd: None,
            timeout: None,
            mode: OutputMode::Raw,
            record: None,
            forward_stdin: false,
        };
        let mut sink = Vec::new();
        assert_eq!(run_exec_with(opts, &|| None, &mut sink), EXIT_INTERNAL);
    }

    #[test]
    fn stdin_pump_thread_spawn_failure_is_propagated_without_running_task() {
        let ran = std::sync::Arc::new(AtomicBool::new(false));
        let task_ran = ran.clone();
        let task: StdinPumpTask = Box::new(move || task_ran.store(true, Ordering::Release));
        let error = spawn_stdin_pump_task_with(task, |_task| {
            Err(std::io::Error::other("synthetic thread exhaustion"))
        })
        .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert!(error.to_string().contains("synthetic thread exhaustion"));
        assert!(!ran.load(Ordering::Acquire));
    }

    #[test]
    fn priority_reply_waits_for_a_started_stdin_message() {
        let none = None;
        let pending_stdin = PendingWrite::new(vec![b'u'; 8192]);
        assert!(priority_reply_may_start(&none, &none));
        assert!(
            !priority_reply_may_start(&none, &pending_stdin),
            "a reply arriving after a partial stdin write must wait for that message"
        );
        let pending_reply = PendingWrite::new(b"reply".to_vec());
        assert!(!priority_reply_may_start(&pending_reply, &none));
    }

    #[test]
    fn admitted_reply_preempts_a_pending_eof_retry() {
        let (reply_tx, reply_rx) = crossbeam_channel::bounded(1);
        let reply_gate = Mutex::new(());
        let mut replies_open = true;
        let mut reply_current = None;
        let mut writes = Vec::new();

        let first = try_eof_after_reply_recheck(
            &reply_gate,
            &reply_rx,
            &mut replies_open,
            &mut reply_current,
            || {
                writes.push(vec![0x04]);
                PtyEofProgress::Pending
            },
        );
        assert_eq!(first, Some(PtyEofProgress::Pending));

        // Stage the arbiter's stale fast-path observation, then publish a
        // terminal reply through the same admission gate used in production.
        assert!(reply_rx.is_empty());
        {
            let _gate = reply_gate.lock().unwrap_or_else(PoisonError::into_inner);
            reply_tx.try_send(b"\x1b[0n".to_vec()).unwrap();
        }

        let second = try_eof_after_reply_recheck(
            &reply_gate,
            &reply_rx,
            &mut replies_open,
            &mut reply_current,
            || {
                writes.push(vec![0x04]);
                PtyEofProgress::Signaled
            },
        );
        assert_eq!(second, None);
        writes.push(reply_current.take().unwrap().bytes);

        let third = try_eof_after_reply_recheck(
            &reply_gate,
            &reply_rx,
            &mut replies_open,
            &mut reply_current,
            || {
                writes.push(vec![0x04]);
                PtyEofProgress::Signaled
            },
        );
        assert_eq!(third, Some(PtyEofProgress::Signaled));
        assert_eq!(
            writes,
            [b"\x04".to_vec(), b"\x1b[0n".to_vec(), b"\x04".to_vec()]
        );
    }

    #[cfg(unix)]
    #[test]
    fn requested_recording_failure_prevents_the_child_from_starting() {
        let temp = kettle_test_support::private_tempdir("kettle-exec-recording-failure-");
        let record = temp.path().join("active.cast");
        let marker = temp.path().join("child-started");
        let active = kettle_core::record::Recorder::start(&record, 80, 24, false).unwrap();
        let opts = ExecOpts {
            argv: vec![
                "sh".into(),
                "-c".into(),
                "printf started > \"$1\"".into(),
                "kettle-exec-test".into(),
                marker.to_string_lossy().into_owned(),
            ],
            cols: 80,
            rows: 24,
            cwd: None,
            timeout: None,
            mode: OutputMode::Raw,
            record: Some(record),
            forward_stdin: false,
        };
        let mut sink = Vec::new();

        assert_eq!(run_exec_with(opts, &|| None, &mut sink), EXIT_INTERNAL);
        assert!(
            !marker.exists(),
            "child ran without its requested audit trace"
        );
        drop(active);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn timeout_kills_a_background_session_member() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("detached.pid");
        let opts = ExecOpts {
            argv: vec![
                "sh".into(),
                "-c".into(),
                "sleep 30 & echo $! > \"$1\"; wait".into(),
                "kettle-exec-test".into(),
                pid_file.to_string_lossy().into_owned(),
            ],
            cols: 80,
            rows: 24,
            cwd: None,
            timeout: Some(Duration::from_millis(150)),
            mode: OutputMode::Raw,
            record: None,
            forward_stdin: false,
        };
        let mut sink = Vec::new();

        assert_eq!(run_exec_with(opts, &|| None, &mut sink), EXIT_TIMEOUT);
        let pid: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let process_path = std::path::PathBuf::from(format!("/proc/{pid}"));
        let deadline = Instant::now() + Duration::from_secs(2);
        while process_path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if process_path.exists() {
            // Avoid leaking the fixture if the assertion fails.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        assert!(
            !process_path.exists(),
            "background session member {pid} survived timeout"
        );
    }

    /// The direct child can exit before the deadline while a background member
    /// of its PTY-created session is reparented and owns a worker which closes
    /// every PTY descriptor. Neither remains reachable from the vanished
    /// root's ancestry, but session membership survives both operations.
    #[cfg(target_os = "linux")]
    #[test]
    fn deadline_after_root_exit_kills_reparented_session_member() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("holder.pid");
        let holder_script = temp.path().join("holder.sh");
        std::fs::write(
            &holder_script,
            r#"#!/bin/sh
sh -c 'echo $$ > "$1"; exec </dev/null >/dev/null 2>&1; sleep 30' worker "$1" &
wait
"#,
        )
        .unwrap();
        let opts = ExecOpts {
            argv: vec![
                "sh".into(),
                "-c".into(),
                r#"sh -c 'trap "" HUP; exec sh "$1" "$2"' holder "$1" "$2" &
                   while [ ! -s "$2" ]; do sleep 0.01; done"#
                    .into(),
                "kettle-exec-test".into(),
                holder_script.to_string_lossy().into_owned(),
                pid_file.to_string_lossy().into_owned(),
            ],
            cols: 80,
            rows: 24,
            cwd: None,
            timeout: Some(Duration::from_millis(400)),
            mode: OutputMode::Raw,
            record: None,
            forward_stdin: false,
        };
        let mut sink = Vec::new();

        assert_eq!(run_exec_with(opts, &|| None, &mut sink), EXIT_TIMEOUT);
        let pid: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let process_path = PathBuf::from(format!("/proc/{pid}"));
        let deadline = Instant::now() + Duration::from_secs(2);
        while process_path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if process_path.exists() {
            // Avoid leaking the fixture if the assertion fails.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGKILL);
            }
        }
        assert!(
            !process_path.exists(),
            "descriptor-free reparented session member {pid} survived deadline teardown"
        );
    }

    /// PTY EOF can precede lossless stdout completion. A later operation
    /// deadline must still have the unreaped root identity needed to
    /// authenticate and kill a descriptor-free session member.
    #[cfg(target_os = "linux")]
    #[test]
    fn blocked_stdout_keeps_the_root_anchor_until_deadline_teardown() {
        struct GateWriter {
            entered: Sender<()>,
            release: Receiver<()>,
        }

        impl Write for GateWriter {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                let _ = self.entered.try_send(());
                self.release
                    .recv_timeout(Duration::from_secs(5))
                    .expect("test releases blocked stdout after lifecycle return");
                Ok(bytes.len())
            }

            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }

        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("holder.pid");
        let (entered_tx, entered_rx) = crossbeam_channel::bounded(1);
        let (release_tx, release_rx) = crossbeam_channel::bounded(1);
        let mut output = WorkerOutput::spawn(
            OutputMode::Raw,
            GateWriter {
                entered: entered_tx,
                release: release_rx,
            },
        )
        .unwrap();
        let opts = ExecOpts {
            argv: vec![
                "sh".into(),
                "-c".into(),
                r#"trap '' HUP
                   sleep 30 </dev/null >/dev/null 2>&1 &
                   echo $! > "$1"
                   printf anchor-output"#
                    .into(),
                "kettle-exec-test".into(),
                pid_file.to_string_lossy().into_owned(),
            ],
            cols: 80,
            rows: 24,
            cwd: None,
            timeout: Some(Duration::from_millis(400)),
            mode: OutputMode::Raw,
            record: None,
            forward_stdin: false,
        };

        let code = run_exec_engine(opts, &|| None, &mut output, None, false);
        assert_eq!(code, EXIT_TIMEOUT);
        entered_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("child output reached the blocked writer");
        release_tx.send(()).unwrap();

        let pid: u32 = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let process_path = PathBuf::from(format!("/proc/{pid}"));
        let deadline = Instant::now() + Duration::from_secs(2);
        while process_path.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        if process_path.exists() {
            unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
        }
        assert!(
            !process_path.exists(),
            "late output deadline lost the root anchor and left session member {pid} alive"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn linux_process_identity_parser_and_scan_cap_are_pinned() {
        let parsed = LinuxProcessIdentity::parse(
            42,
            "42 (a worker ) name) S 1 2 77 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 999 20",
        )
        .expect("parse proc stat with spaces and a close-paren in comm");
        assert_eq!(parsed.pid, 42);
        assert_eq!(parsed.session, 77);
        assert_eq!(parsed.start_time, 999);

        let mut budget = LinuxProcScanBudget::new();
        budget.processes = LINUX_PROC_SCAN_PROCESSES;
        assert!(!budget.process());
        assert!(budget.exhausted);

        let own = LinuxProcessIdentity::read(std::process::id())
            .expect("read own proc stat")
            .expect("own process still exists");
        let mut budget = LinuxProcScanBudget::new();
        let _ = linux_session_processes(own.session, 0, &mut budget);
        assert!(
            budget.exhausted,
            "a local result cap must be reported through the shared budget"
        );

        let expected = LinuxProcessIdentity {
            pid: 42,
            session: 42,
            start_time: 999,
        };
        let stopped = LinuxProcessStat {
            identity: expected,
            state: b'T',
        };
        let zombie = LinuxProcessStat {
            identity: expected,
            state: b'Z',
        };
        let recycled = LinuxProcessStat {
            identity: LinuxProcessIdentity {
                start_time: 1_000,
                ..expected
            },
            state: b'T',
        };
        assert!(linux_root_anchors_numeric_scope(
            Some(expected),
            Some(stopped)
        ));
        assert!(linux_root_anchors_numeric_scope(
            Some(expected),
            Some(zombie)
        ));
        assert!(
            !linux_root_anchors_numeric_scope(Some(expected), None),
            "a vanished leader cannot authenticate a numeric session id"
        );
        assert!(
            !linux_root_anchors_numeric_scope(Some(expected), Some(recycled)),
            "a reused pid with a different start time must fail closed"
        );
        assert!(
            !linux_root_anchors_numeric_scope(None, Some(stopped)),
            "attach failure cannot be upgraded into numeric ownership"
        );
    }

    #[cfg(target_os = "linux")]
    const LINUX_FORK_FIXTURE: &str = "exec::tests::linux_active_fork_session_helper";
    #[cfg(target_os = "linux")]
    const LINUX_FORK_WORKER_FIXTURE: &str = "exec::tests::linux_active_fork_session_worker";

    #[cfg(target_os = "linux")]
    const LEAKED_SLAVE_FIXTURE: &str = "exec::tests::unix_leaked_slave_helper";

    /// Exit the direct child while a descendant still owns the PTY slave. No
    /// reader EOF or parser Exit event can arrive until Kettle tears down that
    /// inherited descriptor, so this is the end-to-end discriminator for the
    /// independent non-reaping process-status observation.
    #[cfg(target_os = "linux")]
    #[test]
    #[allow(clippy::zombie_processes)] // the parent test owns cleanup through exec teardown
    fn unix_leaked_slave_helper() {
        if !invoked_as_fixture(LEAKED_SLAVE_FIXTURE) {
            return;
        }
        use std::os::unix::process::CommandExt as _;

        let mut command = std::process::Command::new("sleep");
        command.arg("30");
        // A same-session descendant is sent SIGHUP when the controlling PTY's
        // leader exits on some Unix kernels. Move this fixture to a new session
        // so it deterministically retains the inherited slave; the parent test
        // owns its explicit cleanup because this is also the documented
        // containment escape that needs an OS-owned supervisor/cgroup.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() < 0 {
                    Err(std::io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        let child = command.spawn().expect("spawn inherited-slave descendant");
        println!("LEAKED_SLAVE_PID {}", child.id());
        std::io::stdout().flush().expect("announce leaked slave");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn direct_child_exit_starts_the_bounded_unix_eof_wait() {
        if invoked_as_fixture(LEAKED_SLAVE_FIXTURE) {
            return;
        }
        let helper = std::env::current_exe().expect("resolve unit-test binary");
        let opts = ExecOpts {
            argv: vec![
                helper.to_string_lossy().into_owned(),
                "--exact".into(),
                LEAKED_SLAVE_FIXTURE.into(),
                "--nocapture".into(),
                "--test-threads=1".into(),
            ],
            cols: 80,
            rows: 24,
            cwd: None,
            timeout: None,
            mode: OutputMode::Raw,
            record: None,
            forward_stdin: false,
        };
        let started = Instant::now();
        let mut sink = Vec::new();
        assert_eq!(run_exec_with(opts, &|| None, &mut sink), EXIT_INTERNAL);
        assert!(
            started.elapsed() < PTY_EOF_TIMEOUT + Duration::from_secs(3),
            "a leaked slave must hit the five-second internal bound, not hang"
        );

        let output = String::from_utf8_lossy(&sink);
        let pid = output
            .split("LEAKED_SLAVE_PID ")
            .nth(1)
            .and_then(|tail| {
                tail.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<libc::pid_t>()
                    .ok()
            })
            .unwrap_or_else(|| panic!("fixture did not announce its descendant: {output:?}"));
        if unsafe { libc::kill(pid, 0) } == 0 {
            // The fixture intentionally called setsid, which is the documented
            // escape from Kettle's Unix PTY-session ownership boundary. The
            // assertion above is about reaching the EOF timeout; clean this
            // deliberately unowned process up explicitly.
            unsafe { libc::kill(pid, libc::SIGKILL) };
        }
    }

    /// Keep creating session members until teardown freezes this process. A
    /// stable collection of sleepers cannot prove the stop acknowledgement:
    /// the bug was specifically a member forking after the final procfs scan.
    #[cfg(target_os = "linux")]
    #[test]
    #[allow(clippy::zombie_processes)] // teardown deliberately kills the whole fixture session
    fn linux_active_fork_session_helper() {
        if !invoked_as_fixture(LINUX_FORK_FIXTURE) {
            return;
        }
        use std::os::unix::process::CommandExt as _;

        let helper = std::env::current_exe().expect("resolve active-fork test binary");
        let mut worker = std::process::Command::new(helper)
            .args([
                "--exact",
                LINUX_FORK_WORKER_FIXTURE,
                "--nocapture",
                "--test-threads=1",
            ])
            // A separate process group in the SAME session is the essential
            // discriminator: the PTY group's fallback signals cannot reach
            // this worker, while the procfs session scan must.
            .process_group(0)
            .spawn()
            .expect("spawn active-fork worker in a separate process group");
        let _ = worker.wait();
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[allow(clippy::zombie_processes)] // teardown deliberately kills the whole fixture session
    fn linux_active_fork_session_worker() {
        if !invoked_as_fixture(LINUX_FORK_WORKER_FIXTURE) {
            return;
        }
        println!("FORK_PID {}", std::process::id());
        let _ = std::io::stdout().flush();
        loop {
            let child = std::process::Command::new("sleep")
                .arg("30")
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("spawn active-fork descendant");
            println!("FORK_PID {}", child.id());
            let _ = std::io::stdout().flush();
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn timeout_freezes_before_the_final_linux_session_scan() {
        if invoked_as_fixture(LINUX_FORK_FIXTURE) {
            return;
        }
        let helper = std::env::current_exe().expect("resolve the unit-test binary");
        let opts = ExecOpts {
            argv: vec![
                helper.to_string_lossy().into_owned(),
                "--exact".into(),
                LINUX_FORK_FIXTURE.into(),
                "--nocapture".into(),
                "--test-threads=1".into(),
            ],
            cols: 80,
            rows: 24,
            cwd: None,
            timeout: Some(Duration::from_millis(300)),
            mode: OutputMode::Raw,
            record: None,
            forward_stdin: false,
        };
        let mut sink = Vec::new();
        assert_eq!(run_exec_with(opts, &|| None, &mut sink), EXIT_TIMEOUT);

        let output = String::from_utf8_lossy(&sink);
        let pids = output
            .split("FORK_PID ")
            .skip(1)
            .filter_map(|tail| {
                tail.chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
                    .parse::<u32>()
                    .ok()
            })
            .collect::<Vec<_>>();
        assert!(
            pids.len() >= 8,
            "fixture must still be forking when teardown starts; output={output:?}"
        );
        let deadline = Instant::now() + Duration::from_secs(2);
        while pids
            .iter()
            .any(|pid| PathBuf::from(format!("/proc/{pid}")).exists())
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        let survivors = pids
            .iter()
            .copied()
            .filter(|pid| PathBuf::from(format!("/proc/{pid}")).exists())
            .collect::<Vec<_>>();
        for pid in &survivors {
            unsafe { libc::kill(*pid as libc::pid_t, libc::SIGKILL) };
        }
        assert!(
            survivors.is_empty(),
            "session members created around the final scan survived: {survivors:?}"
        );
    }

    /// libtest matches `--exact` against a test's FULL path, so the fixture has
    /// to be named the way libtest names it. The integration helpers in
    /// `tests/exec.rs` sit at their crate root and get away with a bare name;
    /// this one is nested, and a filter that fails to match is silent — libtest
    /// runs nothing and exits 0, which reads as a passing child.
    #[cfg(windows)]
    const DESCENDANT_FIXTURE: &str = "exec::tests::windows_descendant_job_helper";

    /// Whether this test binary was re-executed to act as a fixture rather than
    /// run as part of an ordinary suite. Reading argv is race-free, unlike the
    /// environment-variable guard the integration fixtures use — those are set
    /// on a `Command` by their parent, which `run_exec_with` has no way to do.
    #[cfg(any(windows, target_os = "linux"))]
    fn invoked_as_fixture(name: &str) -> bool {
        let mut exact = false;
        let mut named = false;
        for arg in std::env::args().skip(1) {
            exact |= arg == "--exact";
            named |= arg == name;
        }
        exact && named
    }

    /// The child half of `timeout_terminates_a_windows_descendant_job`: own a
    /// grandchild, announce it, then block until Kettle's timeout ends the job.
    ///
    /// This test binary is its own fixture because re-executing it costs
    /// milliseconds. The fixture this replaced launched `powershell.exe`, whose
    /// cold start on a loaded hosted runner regularly outlasted the very
    /// timeout under test — so the parent fired before the fixture could name
    /// its descendant, and the test failed having proven nothing. Raising the
    /// timeout had already been tried; it moves the race rather than removing
    /// it, because the fixture's setup and the deadline share one budget.
    #[cfg(windows)]
    #[test]
    fn windows_descendant_job_helper() {
        if !invoked_as_fixture(DESCENDANT_FIXTURE) {
            return;
        }
        let mut descendant = std::process::Command::new("ping.exe")
            .args(["-n", "30", "127.0.0.1"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("spawn the descendant to be job-terminated");
        // Stdout is the whole channel back, so no temp path has to be agreed
        // on and no environment variable has to be mutated mid-suite.
        println!("DESCENDANT_PID {}", descendant.id());
        let _ = std::io::stdout().flush();
        // Kettle's timeout is what should end this. Waiting on the descendant
        // also bounds a stray manual invocation instead of hanging forever.
        let _ = descendant.wait();
    }

    #[cfg(windows)]
    #[test]
    fn timeout_terminates_a_windows_descendant_job() {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
        };

        // The child binary contains THIS test too. If its filter ever stopped
        // matching, libtest would run the whole suite in the child, reach here,
        // and re-exec again. Refuse to be that child.
        if invoked_as_fixture(DESCENDANT_FIXTURE) {
            return;
        }

        let helper = std::env::current_exe().expect("resolve the unit-test binary");
        let opts = ExecOpts {
            argv: vec![
                helper.to_string_lossy().into_owned(),
                // `--exact` first, then the filter, matching the integration
                // fixtures in `tests/exec.rs`. libtest's parser does not care,
                // but one spelling across the repo is one thing to get right.
                "--exact".into(),
                DESCENDANT_FIXTURE.into(),
                "--nocapture".into(),
                "--test-threads=1".into(),
            ],
            cols: 80,
            rows: 24,
            cwd: None,
            // The fixture announces its descendant within milliseconds of
            // starting, so this budget is margin rather than a race.
            timeout: Some(Duration::from_secs(5)),
            mode: OutputMode::Raw,
            record: None,
            forward_stdin: false,
        };
        let mut sink = Vec::new();

        assert_eq!(run_exec_with(opts, &|| None, &mut sink), EXIT_TIMEOUT);
        let output = String::from_utf8_lossy(&sink);
        // Take only the leading digit run: PTY output continues past the
        // marker, and it carries CRLF plus whatever libtest prints afterwards.
        let pid: u32 = output
            .split("DESCENDANT_PID ")
            .nth(1)
            .map(|tail| {
                tail.trim_start()
                    .chars()
                    .take_while(char::is_ascii_digit)
                    .collect::<String>()
            })
            .and_then(|digits| digits.parse().ok())
            .unwrap_or_else(|| {
                panic!("fixture must announce its descendant pid before timeout; output={output:?}")
            });
        // SAFETY: OpenProcess receives a recorded positive pid and only the
        // synchronization right. The returned handle is closed below.
        let process = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        if process.is_null() {
            return; // The process object was already fully reaped.
        }
        let exited = unsafe { WaitForSingleObject(process, 1000) } == WAIT_OBJECT_0;
        unsafe { CloseHandle(process) };
        assert!(exited, "Windows descendant {pid} survived Job termination");
    }

    #[test]
    fn cancellable_capture_kills_child_and_finishes_recorder_promptly() {
        let dir = kettle_test_support::private_tempdir("kettle-exec-cancel-");
        let record = dir.path().join("cancel.cast");
        #[cfg(unix)]
        let argv = vec![
            "sh".into(),
            "-c".into(),
            "printf started; exec sleep 30".into(),
        ];
        #[cfg(windows)]
        let argv = vec!["ping".into(), "-n".into(), "30".into(), "127.0.0.1".into()];
        let opts = ExecOpts {
            argv,
            cols: 80,
            rows: 24,
            cwd: None,
            timeout: Some(Duration::from_secs(30)),
            mode: OutputMode::StripAnsi,
            record: Some(record.clone()),
            forward_stdin: false,
        };
        let cancelled = std::sync::Arc::new(AtomicBool::new(false));
        let child_cancelled = cancelled.clone();
        let started = Instant::now();
        let worker = std::thread::spawn(move || {
            run_exec_capture_cancellable(opts, child_cancelled.as_ref())
        });
        std::thread::sleep(Duration::from_millis(150));
        cancelled.store(true, Ordering::Release);
        let (code, _output) = worker.join().unwrap();

        assert_eq!(code, EXIT_CANCELLED);
        assert!(started.elapsed() < Duration::from_secs(5));
        let cast = std::fs::read_to_string(&record).unwrap();
        assert!(
            cast.lines()
                .next()
                .is_some_and(|line| line.contains("\"version\":2"))
        );
    }

    #[test]
    fn exec_module_is_headless_no_ui_no_winit() {
        // Drift guard: the headless exec engine must NOT pull in the GUI. If a
        // future edit reaches for a `kettle_ui::` / `winit::` path, this fails —
        // "headless means headless". Scans for the `::` *usage* forms so the
        // module's own doc comment (which names the crates in prose) doesn't
        // trip it; `production_source` strips every test item so the assertions
        // cannot satisfy their own checks.
        let scan = super::production_source();
        assert!(
            !scan.contains("kettle_ui::"),
            "exec.rs must not use kettle_ui (headless)"
        );
        assert!(
            !scan.contains("winit::") && !scan.contains("use winit"),
            "exec.rs must not use winit (headless)"
        );
    }

    #[test]
    fn json_start_event_is_emitted() {
        // Without a PTY (None probe, empty-ish), exercise the Outputter start.
        let mut o = Outputter::new(OutputMode::Json);
        let mut sink = Vec::new();
        o.start(&mut sink, 80, 24).unwrap();
        let s = String::from_utf8(sink).unwrap();
        assert!(s.contains("\"event\":\"start\""), "got: {s}");
        assert!(s.contains("\"cols\":80"));
    }

    struct ErrorSink {
        fail_write: bool,
        fail_flush: bool,
    }

    impl Write for ErrorSink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if self.fail_write {
                Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "test write failure",
                ))
            } else {
                Ok(bytes.len())
            }
        }

        fn flush(&mut self) -> std::io::Result<()> {
            if self.fail_flush {
                Err(std::io::Error::new(
                    std::io::ErrorKind::StorageFull,
                    "test flush failure",
                ))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn outputter_propagates_write_and_flush_failures() {
        for mode in [OutputMode::Raw, OutputMode::StripAnsi, OutputMode::Json] {
            let mut outputter = Outputter::new(mode);
            let mut sink = ErrorSink {
                fail_write: true,
                fail_flush: false,
            };
            assert_eq!(
                outputter.output(&mut sink, b"data").unwrap_err().kind(),
                std::io::ErrorKind::BrokenPipe
            );
        }

        let mut outputter = Outputter::new(OutputMode::Json);
        let mut sink = ErrorSink {
            fail_write: false,
            fail_flush: true,
        };
        assert_eq!(
            outputter
                .finish(&mut sink, 0, Duration::ZERO)
                .unwrap_err()
                .kind(),
            std::io::ErrorKind::StorageFull
        );
    }

    #[test]
    fn explicit_exec_cwd_validation_rejects_missing_paths_and_files() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("missing");
        assert!(validate_exec_cwd(Some(&missing)).is_err());

        let file = temp.path().join("file");
        std::fs::write(&file, b"not a directory").unwrap();
        assert!(validate_exec_cwd(Some(&file)).is_err());
        assert_eq!(
            validate_exec_cwd(Some(temp.path())).unwrap(),
            temp.path().to_str()
        );
    }

    /// The child stream must not travel through the process-global stdout
    /// buffer. Anything left there by a failed write is retried by the
    /// runtime's exit-time flush on the main thread, long after exec has
    /// chosen its exit code.
    #[cfg(unix)]
    #[test]
    fn exec_stdout_sink_is_a_private_descriptor() {
        use std::os::fd::AsRawFd as _;

        let sink = exec_stdout_sink().expect("duplicate stdout");
        assert_ne!(
            sink.as_raw_fd(),
            1,
            "exec must own a duplicate, not descriptor 1 itself"
        );
    }

    /// The end-to-end guarantee — a broken stdout yields exit 74 rather than
    /// death by SIGPIPE — belongs to
    /// `exec_reports_a_broken_stdout_and_reaps_the_child` in `tests/exec.rs`,
    /// which runs Kettle as its own process. It cannot be proven from here:
    /// showing that SIG_DFL becomes SIG_IGN means making SIGPIPE briefly fatal
    /// process-wide, which would kill unrelated parallel tests that write to
    /// pipes. What is safe to pin is the direction: the commitment must never
    /// leave the signal fatal.
    #[cfg(unix)]
    #[test]
    fn committing_an_output_failure_exit_never_leaves_sigpipe_fatal() {
        commit_output_failure_exit();

        // SAFETY: a null `act` queries the disposition without changing it, so
        // this observes process state that other tests share but never mutates
        // it. `oldact` is fully initialized by the call.
        let disposition = unsafe {
            let mut current: libc::sigaction = std::mem::zeroed();
            assert_eq!(
                libc::sigaction(libc::SIGPIPE, std::ptr::null(), &mut current),
                0,
                "querying the SIGPIPE disposition must succeed"
            );
            current.sa_sigaction
        };
        assert_eq!(
            disposition,
            libc::SIG_IGN,
            "a committed output failure must leave SIGPIPE ignored"
        );
    }
}

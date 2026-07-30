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
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use kettle_core::{
    CursorShape, PtyEofProgress, PtyGeometry, PtyOutputSender, PtyStdin, TermEvent, Terminal,
    TerminalCapabilities, Waker,
};

/// How long to keep draining output after the child exits before we stop and
/// report the code. Doubles as the ConPTY late-repaint mitigation: ConPTY's
/// screen-differ can emit a final paint after the child is gone. Same order of
/// magnitude as the dev-record reap settle.
const SETTLE: Duration = Duration::from_millis(60);
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

/// Exit code for `--timeout` expiry (coreutils `timeout(1)` convention).
pub const EXIT_TIMEOUT: i32 = 124;
/// Exit code for an internal kettle error (spawn failure, no PTY, bad args).
pub const EXIT_INTERNAL: i32 = 125;
/// Internal exit status when an MCP request cancels a running headless child.
pub const EXIT_CANCELLED: i32 = 130;

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
}

#[derive(Debug, Default)]
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
                StripState::Escape => match b {
                    b'[' => Self::csi(),
                    b']' => Self::string(true),
                    b'P' | b'X' | b'^' | b'_' => Self::string(false),
                    0x20..=0x2f => StripState::EscapeIntermediate {
                        remaining: MAX_CONTROL_SEQUENCE_BYTES,
                    },
                    0x1b => StripState::Escape,
                    _ => StripState::Ground,
                },
                StripState::EscapeIntermediate { remaining } => {
                    if b == 0x1b {
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
                    if b == 0x1b {
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
                    if b == 0x9c
                        || (bel_terminated && b == 0x07)
                        || (escaped && b == b'\\')
                        || remaining <= 1
                    {
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

    fn string(bel_terminated: bool) -> StripState {
        StripState::String {
            bel_terminated,
            escaped: false,
            remaining: MAX_CONTROL_SEQUENCE_BYTES,
        }
    }
}

/// Run `kettle exec` end to end; returns the process exit code to propagate.
pub fn run_exec(opts: ExecOpts) -> i32 {
    let mut output = match WorkerOutput::spawn(opts.mode, std::io::stdout()) {
        Ok(output) => output,
        Err(error) => {
            let _ = writeln!(
                std::io::stderr(),
                "kettle exec: cannot start stdout writer: {error}"
            );
            return EXIT_INTERNAL;
        }
    };
    run_exec_engine(opts, &default_size_probe, &mut output, None)
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

fn run_exec_capture_inner(opts: ExecOpts, cancelled: Option<&AtomicBool>) -> (i32, String) {
    /// A sink that keeps only the last `cap` bytes (so an unbounded producer
    /// can't exhaust memory; agents want "what just happened" anyway).
    struct TailSink {
        buf: Vec<u8>,
        cap: usize,
    }
    impl Write for TailSink {
        fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
            self.buf.extend_from_slice(data);
            if self.buf.len() > self.cap {
                let drop = self.buf.len() - self.cap;
                self.buf.drain(..drop);
            }
            Ok(data.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut sink = TailSink {
        buf: Vec::new(),
        cap: 1024 * 1024,
    };
    let mut output = DirectOutput::new(opts.mode, &mut sink);
    let code = run_exec_engine(opts, &default_size_probe, &mut output, cancelled);
    (code, String::from_utf8_lossy(&sink.buf).into_owned())
}

/// Default console-size probe (real terminal dimensions when stdout is a TTY).
pub fn default_size_probe() -> Option<(u16, u16)> {
    terminal_size_cols_rows()
}

/// Core run loop, with the stdout sink and size probe injected for testing.
#[cfg(test)]
pub fn run_exec_with(
    opts: ExecOpts,
    _size_probe: &dyn Fn() -> Option<(u16, u16)>,
    sink: &mut dyn Write,
) -> i32 {
    let mut output = DirectOutput::new(opts.mode, sink);
    run_exec_engine(opts, _size_probe, &mut output, None)
}

/// Drain one bounded output slice and report whether a backlog remains.
///
/// A bounded channel alone does not make an unbounded `try_recv` loop fair:
/// the producer can refill each slot as soon as the owner removes it. Both a
/// message and byte budget keep lifecycle checks reachable under that race.
fn drain_output_slice(
    receiver: &Receiver<Vec<u8>>,
    recorder: &mut Option<kettle_core::record::Recorder>,
    output: &mut dyn ExecOutput,
) -> bool {
    let mut bytes_drained = 0usize;
    for _ in 0..OUTPUT_SLICE_MESSAGES {
        if bytes_drained >= OUTPUT_SLICE_BYTES {
            break;
        }
        if !output.ready() {
            return true;
        }
        let Ok(bytes) = receiver.try_recv() else {
            return false;
        };
        bytes_drained = bytes_drained.saturating_add(bytes.len());
        record_chunk(recorder, &bytes);
        output.output(bytes);
    }
    !receiver.is_empty() || !output.ready()
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

fn run_exec_engine(
    mut opts: ExecOpts,
    _size_probe: &dyn Fn() -> Option<(u16, u16)>,
    output: &mut dyn ExecOutput,
    cancelled: Option<&AtomicBool>,
) -> i32 {
    if opts.argv.is_empty() {
        let _ = writeln!(std::io::stderr(), "kettle exec: no command given");
        return EXIT_INTERNAL;
    }
    // Clamp geometry into a sane PTY range (kettle-core clamps too, but a 0
    // here would make ConPTY unhappy).
    opts.cols = opts.cols.clamp(1, u16::MAX);
    opts.rows = opts.rows.clamp(1, u16::MAX);

    let (tx, rx): (Sender<TermEvent>, Receiver<TermEvent>) =
        crossbeam_channel::bounded(PTY_EVENT_QUEUE_DEPTH);
    let (otx, orx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = crossbeam_channel::bounded(4);
    let (stdin_tx, stdin_rx) = crossbeam_channel::bounded::<StdinPumpEvent>(4);
    let (pty_reply_tx, pty_reply_rx) = crossbeam_channel::bounded::<Vec<u8>>(64);
    let pty_reply_gate = Arc::new(Mutex::new(()));
    let (stdin_done_tx, stdin_done_rx) = crossbeam_channel::unbounded::<StdinPumpResult>();
    let waker: Waker = std::sync::Arc::new(|| {});
    let cwd = opts.cwd.as_ref().and_then(|p| p.to_str());

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

    let term = match Terminal::new_with_env_and_output_geometry_and_capabilities(
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
        TerminalCapabilities { osc52_copy: false },
        tx,
        waker,
        Some(PtyOutputSender::lossless(otx)),
    ) {
        Ok(t) => t,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "kettle exec: cannot start PTY: {e}");
            return EXIT_INTERNAL;
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

    output.start(opts.cols, opts.rows);

    let started = Instant::now();
    let mut child_gone_at: Option<Instant> = None;
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
        let lifecycle_stop = if child_gone_at.is_some() {
            None
        } else if cancellation_requested {
            Some(EXIT_CANCELLED)
        } else if timeout_expired {
            Some(EXIT_TIMEOUT)
        } else {
            None
        };
        if let Some(code) = lifecycle_stop {
            log::debug!(
                "kettle exec {} reached; starting bounded teardown",
                if code == EXIT_CANCELLED {
                    "cancellation"
                } else {
                    "timeout"
                }
            );
            process_tree.terminate(&term);
            if code == EXIT_CANCELLED {
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
            if let Some(mut recorder) = recorder.take() {
                recorder.finish();
            }
            output.finish(code, started.elapsed(), OutputFinish::AbandonPending);
            log::debug!("kettle exec bounded stop teardown finished");
            return code;
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
        let output_backlog = drain_output_slice(&orx, &mut recorder, output);
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
        let event_backlog = if output.ready() {
            drain_event_slice_until(&rx, |ev| {
                match ev {
                    TermEvent::PtyWrite(s) => queue_reply(s.as_bytes()),
                    TermEvent::Title(t) => output.title(t),
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
                        log::debug!("kettle exec handling child-exit event");
                        child_gone_at.get_or_insert_with(Instant::now);
                    }
                    _ => {}
                }
                output.ready()
            })
        } else {
            !rx.is_empty()
        };
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
            process_tree.terminate(&term);
            let _ = wait_for_exit_code(&term);
            std::thread::sleep(SETTLE);
            let _ = drain_output_slice(&orx, &mut recorder, output);
            if let Some(mut recorder) = recorder.take() {
                recorder.finish();
            }
            output.finish(EXIT_INTERNAL, started.elapsed(), OutputFinish::Complete);
            return EXIT_INTERNAL;
        }
        if let Some(error) = stdin_forwarding_error {
            log::debug!("kettle exec checking child after PTY forwarding error");
            if child_gone_at.is_none() && term.child_exited() {
                child_gone_at = Some(Instant::now());
            }
            if child_gone_at.is_none() {
                let _ = writeln!(
                    std::io::stderr(),
                    "kettle exec: cannot safely write child PTY input: {error}"
                );
                process_tree.terminate(&term);
                let _ = wait_for_exit_code(&term);
                std::thread::sleep(SETTLE);
                let _ = drain_output_slice(&orx, &mut recorder, output);
                if let Some(mut recorder) = recorder.take() {
                    recorder.finish();
                }
                output.finish(EXIT_INTERNAL, started.elapsed(), OutputFinish::Complete);
                return EXIT_INTERNAL;
            }
            // EIO/BrokenPipe is the normal final write outcome when a
            // short-lived child exits before consuming all piped input. Its
            // authoritative process status wins, but lifecycle handling below
            // must still run this turn.
        }

        // Exit detection: poll the real child status (authoritative), then
        // settle-drain so trailing/late output (ConPTY repaint) is captured.
        let child_exited = child_gone_at.is_none() && term.child_exited();
        if trace_lifecycle {
            log::debug!("kettle exec lifecycle child-status poll returned: exited={child_exited}");
        }
        if child_exited {
            child_gone_at = Some(Instant::now());
        }
        if let Some(gone) = child_gone_at
            && gone.elapsed() >= SETTLE
            && orx.is_empty()
        {
            // Final drain in case something landed in the settle window.
            let _ = drain_output_slice(&orx, &mut recorder, output);
            if let Some(mut r) = recorder.take() {
                r.finish();
            }
            // The VT `Exit` event can arrive a hair before the OS has reaped the
            // child, so `child_exit_code()` may still be `None` here. Poll it
            // briefly rather than defaulting to 0 (which would report a FAILED
            // child as success); fall back to a non-zero sentinel only if the
            // status never materializes.
            let code = wait_for_exit_code(&term)
                .map(clamp_code)
                .unwrap_or(EXIT_INTERNAL);
            output.finish(code, started.elapsed(), OutputFinish::Complete);
            return code;
        }

        let output_blocked = !output.ready();
        if (output_backlog || event_backlog) && !output_blocked {
            // Preserve throughput under a real backlog without paying the idle
            // polling delay, now that lifecycle checks have had a turn.
            std::thread::yield_now();
            continue;
        }
        std::thread::sleep(Duration::from_millis(8));
    }
}

/// Stop the complete command tree for timeout/cancellation. Killing only the
/// PTY's immediate child leaves backgrounded or `setsid` descendants running.
/// Linux exposes the parent relation through `/proc`; freeze the discovered
/// tree before killing it so descendants cannot race by forking while it is
/// being enumerated. Other Unix targets still kill the PTY process group, and
/// every platform finishes with the portable-pty child handle.
struct ExecProcessTree {
    #[cfg(windows)]
    job: Option<WindowsJob>,
}

impl ExecProcessTree {
    fn attach(_term: &Terminal) -> Self {
        #[cfg(windows)]
        let job = _term
            .child_pid()
            .and_then(|pid| match WindowsJob::attach(pid) {
                Ok(job) => Some(job),
                Err(error) => {
                    log::warn!("kettle exec could not attach child {pid} to a Job Object: {error}");
                    None
                }
            });
        Self {
            #[cfg(windows)]
            job,
        }
    }

    fn terminate(&self, term: &Terminal) {
        let Some(root) = term.child_pid() else {
            term.kill();
            return;
        };

        #[cfg(target_os = "linux")]
        {
            let mut frozen = std::collections::HashSet::new();
            // SAFETY: root is the positive PID returned by portable-pty.
            unsafe {
                libc::kill(root as libc::pid_t, libc::SIGSTOP);
            }
            frozen.insert(root);
            // A small fixed-point loop bounds work under a hostile fork load while
            // freezing every process as soon as it becomes visible.
            for _ in 0..8 {
                let mut discovered = linux_descendants(root, 4096);
                discovered.push(root);
                let before = frozen.len();
                for pid in discovered {
                    if frozen.insert(pid) {
                        // SAFETY: kill is called with a positive PID obtained from
                        // procfs. Failure (already exited or permission denied) is
                        // harmless and the portable child kill remains below.
                        unsafe {
                            libc::kill(pid as libc::pid_t, libc::SIGSTOP);
                        }
                    }
                }
                if frozen.len() == before {
                    break;
                }
            }
            for pid in frozen.iter().copied().filter(|pid| *pid != root) {
                // SAFETY: as above; SIGKILL cannot invoke user handlers.
                unsafe {
                    libc::kill(pid as libc::pid_t, libc::SIGKILL);
                }
            }
        }

        #[cfg(all(unix, not(target_os = "linux")))]
        {
            // portable-pty makes the child the leader of its terminal process
            // group on Unix. A negative pid addresses that entire group.
            if let Ok(group) = libc::pid_t::try_from(root) {
                // SAFETY: group is a positive child PID; negating it selects only
                // that child's process group.
                unsafe {
                    libc::kill(-group, libc::SIGKILL);
                }
            }
        }

        #[cfg(windows)]
        if let Some(job) = &self.job {
            log::debug!("kettle exec terminating Windows Job Object");
            if let Err(error) = job.terminate() {
                log::warn!("kettle exec could not terminate its Windows Job Object: {error}");
            }
            log::debug!("kettle exec finished Windows Job Object termination");
        }

        #[cfg(not(unix))]
        let _ = root;

        log::debug!("kettle exec terminating direct PTY child");
        term.kill();
        log::debug!("kettle exec finished direct PTY child termination");
    }
}

#[cfg(windows)]
struct WindowsJob(windows_sys::Win32::Foundation::HANDLE);

#[cfg(windows)]
impl WindowsJob {
    fn attach(pid: u32) -> std::io::Result<Self> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            AssignProcessToJobObject, CreateJobObjectW, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
            SetInformationJobObject,
        };
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
        };

        // SAFETY: null attributes/name request a private unnamed job handle.
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if job.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        // SAFETY: `limits` has the exact structure and size required for the
        // selected information class, and `job` remains valid for the call.
        if unsafe {
            SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                std::ptr::addr_of!(limits).cast(),
                std::mem::size_of_val(&limits) as u32,
            )
        } == 0
        {
            let error = std::io::Error::last_os_error();
            unsafe { CloseHandle(job) };
            return Err(error);
        }
        // PROCESS_SET_QUOTA and PROCESS_TERMINATE are the documented rights
        // required by AssignProcessToJobObject.
        let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid) };
        if process.is_null() {
            let error = std::io::Error::last_os_error();
            unsafe { CloseHandle(job) };
            return Err(error);
        }
        let assigned = unsafe { AssignProcessToJobObject(job, process) };
        unsafe { CloseHandle(process) };
        if assigned == 0 {
            let error = std::io::Error::last_os_error();
            unsafe { CloseHandle(job) };
            return Err(error);
        }
        Ok(Self(job))
    }

    fn terminate(&self) -> std::io::Result<()> {
        use windows_sys::Win32::System::JobObjects::TerminateJobObject;

        // SAFETY: self owns a valid job handle until Drop. Termination reaches
        // every process assigned directly or inherited through the job.
        if unsafe { TerminateJobObject(self.0, EXIT_CANCELLED as u32) } == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        // SAFETY: WindowsJob exclusively owns this handle.
        unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0) };
    }
}

#[cfg(target_os = "linux")]
fn linux_descendants(root: u32, limit: usize) -> Vec<u32> {
    let mut result = Vec::new();
    let mut pending = vec![root];
    let mut seen = std::collections::HashSet::from([root]);
    while let Some(parent) = pending.pop() {
        if result.len() >= limit {
            break;
        }
        let path = format!("/proc/{parent}/task/{parent}/children");
        let Ok(children) = std::fs::read_to_string(path) else {
            continue;
        };
        for child in children
            .split_ascii_whitespace()
            .filter_map(|value| value.parse::<u32>().ok())
        {
            if seen.insert(child) {
                result.push(child);
                pending.push(child);
                if result.len() >= limit {
                    break;
                }
            }
        }
    }
    result
}

fn record_chunk(recorder: &mut Option<kettle_core::record::Recorder>, bytes: &[u8]) {
    use kettle_core::record::RecordStatus;

    let Some(recorder) = recorder.as_mut() else {
        return;
    };
    let previous = recorder.status();
    recorder.record_output(bytes);
    if previous == RecordStatus::Recording && recorder.status() != previous {
        let reason = match recorder.status() {
            RecordStatus::LimitReached => "512 MiB session limit reached",
            RecordStatus::IoError => "recording I/O failed",
            RecordStatus::Recording => return,
        };
        let _ = writeln!(
            std::io::stderr(),
            "kettle exec: asciicast capture stopped ({reason}); child execution continues"
        );
    }
}

/// Map a child's raw exit code into the code this process should report.
///
/// On Unix `std::process::exit` only keeps the low 8 bits, and portable-pty
/// folds signal death into the code there, so we mask to 0..=255 — the value we
/// log then matches what the shell would see. On Windows the full 32-bit code
/// is meaningful (children routinely exit with codes outside 0..=255, e.g.
/// `STATUS_ACCESS_VIOLATION` 0xC0000005), so we pass it through, saturating into
/// `i32` rather than truncating to one byte.
fn clamp_code(code: u32) -> i32 {
    #[cfg(unix)]
    {
        (code & 0xff) as i32
    }
    #[cfg(windows)]
    {
        code.min(i32::MAX as u32) as i32
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
    let deadline = Instant::now() + Duration::from_millis(250);
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
    /// Ordinary completion is lossless: drain every admitted command and wait
    /// for the final flush before returning the child's status.
    Complete,
    /// Timeout/cancellation must not wait for a stalled stdout consumer.
    AbandonPending,
}

trait ExecOutput {
    /// Try to publish the one lifecycle-owned pending command.
    fn ready(&mut self) -> bool;
    fn start(&mut self, cols: u16, rows: u16);
    fn output(&mut self, bytes: Vec<u8>);
    fn title(&mut self, title: String);
    fn finish(&mut self, code: i32, duration: Duration, mode: OutputFinish);
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
    fn ready(&mut self) -> bool {
        true
    }

    fn start(&mut self, cols: u16, rows: u16) {
        self.outputter.start(self.sink, cols, rows);
    }

    fn output(&mut self, bytes: Vec<u8>) {
        self.outputter.output(self.sink, &bytes);
    }

    fn title(&mut self, title: String) {
        self.outputter.title(self.sink, &title);
    }

    fn finish(&mut self, code: i32, duration: Duration, _mode: OutputFinish) {
        self.outputter.finish(self.sink, code, duration);
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
    worker: Option<std::thread::JoinHandle<()>>,
}

impl WorkerOutput {
    fn spawn(mode: OutputMode, mut sink: impl Write + Send + 'static) -> std::io::Result<Self> {
        let (sender, receiver) = crossbeam_channel::bounded(OUTPUT_WRITER_QUEUE_DEPTH);
        let worker = std::thread::Builder::new()
            .name("kettle-stdout-writer".into())
            .spawn(move || {
                let mut outputter = Outputter::new(mode);
                while let Ok(command) = receiver.recv() {
                    match command {
                        OutputCommand::Start { cols, rows } => {
                            outputter.start(&mut sink, cols, rows);
                        }
                        OutputCommand::Output(bytes) => {
                            outputter.output(&mut sink, &bytes);
                        }
                        OutputCommand::Title(title) => {
                            outputter.title(&mut sink, &title);
                        }
                        OutputCommand::Finish { code, duration } => {
                            outputter.finish(&mut sink, code, duration);
                            break;
                        }
                    }
                }
            })?;
        Ok(Self {
            mode,
            sender: Some(sender),
            pending: None,
            worker: Some(worker),
        })
    }

    fn try_dispatch(&mut self, command: OutputCommand) {
        debug_assert!(self.pending.is_none());
        let Some(sender) = self.sender.as_ref() else {
            return;
        };
        match sender.try_send(command) {
            Ok(()) => {}
            Err(crossbeam_channel::TrySendError::Full(command)) => {
                self.pending = Some(command);
            }
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                log::error!("kettle exec stdout writer stopped unexpectedly");
            }
        }
    }

    fn finish_complete(&mut self, code: i32, duration: Duration) {
        if let Some(command) = self.pending.take()
            && self
                .sender
                .as_ref()
                .is_some_and(|sender| sender.send(command).is_err())
        {
            log::error!("kettle exec stdout writer stopped with pending output");
        }
        if self.sender.as_ref().is_some_and(|sender| {
            sender
                .send(OutputCommand::Finish { code, duration })
                .is_err()
        }) {
            log::error!("kettle exec stdout writer stopped before final flush");
        }
        drop(self.sender.take());
        if let Some(worker) = self.worker.take()
            && worker.join().is_err()
        {
            log::error!("kettle exec stdout writer panicked");
        }
    }

    fn finish_abandoning_pending(&mut self, code: i32, duration: Duration) {
        if self.pending.is_none()
            && let Some(sender) = self.sender.as_ref()
        {
            let _ = sender.try_send(OutputCommand::Finish { code, duration });
        }

        // Chosen timeout/cancellation contract: commands already accepted by
        // the worker may complete if the consumer resumes immediately, but the
        // lifecycle never waits. The lifecycle-owned pending command, any raw
        // PTY tail not admitted to stdout, and a final JSON exit event that
        // cannot enter the full queue are abandoned explicitly. `main` then
        // calls `process::exit`, which terminates a writer still blocked in the
        // OS. Ordinary completion uses `finish_complete` and drops none.
        self.pending = None;
        drop(self.sender.take());
        drop(self.worker.take());
    }
}

impl ExecOutput for WorkerOutput {
    fn ready(&mut self) -> bool {
        let Some(command) = self.pending.take() else {
            return true;
        };
        let Some(sender) = self.sender.as_ref() else {
            return true;
        };
        match sender.try_send(command) {
            Ok(()) => true,
            Err(crossbeam_channel::TrySendError::Full(command)) => {
                self.pending = Some(command);
                false
            }
            Err(crossbeam_channel::TrySendError::Disconnected(_)) => {
                log::error!("kettle exec stdout writer stopped unexpectedly");
                true
            }
        }
    }

    fn start(&mut self, cols: u16, rows: u16) {
        if self.mode == OutputMode::Json {
            self.try_dispatch(OutputCommand::Start { cols, rows });
        }
    }

    fn output(&mut self, bytes: Vec<u8>) {
        self.try_dispatch(OutputCommand::Output(bytes));
    }

    fn title(&mut self, title: String) {
        if self.mode == OutputMode::Json {
            self.try_dispatch(OutputCommand::Title(title));
        }
    }

    fn finish(&mut self, code: i32, duration: Duration, mode: OutputFinish) {
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

    fn start(&mut self, sink: &mut dyn Write, cols: u16, rows: u16) {
        if self.mode == OutputMode::Json {
            let v = serde_json::json!({"v":1,"event":"start","cols":cols,"rows":rows});
            let _ = writeln!(sink, "{v}");
            let _ = sink.flush();
        }
    }

    fn output(&mut self, sink: &mut dyn Write, bytes: &[u8]) {
        match self.mode {
            OutputMode::Raw => {
                let _ = sink.write_all(bytes);
                let _ = sink.flush();
            }
            OutputMode::StripAnsi => {
                self.scratch.clear();
                self.stripper.push(bytes, &mut self.scratch);
                let _ = sink.write_all(&self.scratch);
                let _ = sink.flush();
            }
            OutputMode::Json => {
                let mut data = String::new();
                push_utf8_streaming(&mut self.utf8_carry, bytes, &mut data);
                if data.is_empty() {
                    return; // only an incomplete sequence so far — wait for more
                }
                let v = serde_json::json!({"v":1,"event":"output","data":data});
                let _ = writeln!(sink, "{v}");
                let _ = sink.flush();
            }
        }
    }

    fn title(&mut self, sink: &mut dyn Write, title: &str) {
        if self.mode == OutputMode::Json {
            let v = serde_json::json!({"v":1,"event":"title","data":title});
            let _ = writeln!(sink, "{v}");
            let _ = sink.flush();
        }
    }

    fn finish(&mut self, sink: &mut dyn Write, code: i32, dur: Duration) {
        if self.mode == OutputMode::Json {
            // v2.27.0 (audit): flush any trailing incomplete UTF-8 sequence
            // lossily before the exit event, so a stream that ends mid-codepoint
            // doesn't silently drop its final bytes.
            if !self.utf8_carry.is_empty() {
                let data = String::from_utf8_lossy(&self.utf8_carry).into_owned();
                self.utf8_carry.clear();
                let v = serde_json::json!({"v":1,"event":"output","data":data});
                let _ = writeln!(sink, "{v}");
            }
            let v = serde_json::json!({
                "v":1,"event":"exit","code":code,"duration_ms":dur.as_millis() as u64
            });
            let _ = writeln!(sink, "{v}");
        }
        let _ = sink.flush();
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
                if reply_current.is_none() && replies_open {
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

                let writing_reply = reply_current.is_some();
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
                            // Wait briefly for a newly generated high-priority
                            // reply before retrying the pending frame.
                            if replies_open && !writing_reply {
                                match replies.recv_timeout(Duration::from_millis(1)) {
                                    Ok(bytes) => {
                                        reply_current = PendingWrite::new(bytes);
                                    }
                                    Err(crossbeam_channel::RecvTimeoutError::Disconnected) => {
                                        replies_open = false;
                                    }
                                    Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
                                }
                            } else {
                                std::thread::sleep(Duration::from_millis(1));
                            }
                        }
                        Ok(written) if writing_reply => {
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
    use super::*;

    #[test]
    fn output_slice_bounds_a_continuously_refilled_channel() {
        let (sender, receiver) = crossbeam_channel::unbounded();
        for _ in 0..=OUTPUT_SLICE_MESSAGES {
            sender.send(vec![b'x']).unwrap();
        }
        let mut recorder = None;
        let mut sink = Vec::new();
        let mut output = DirectOutput::new(OutputMode::Raw, &mut sink);

        assert!(drain_output_slice(&receiver, &mut recorder, &mut output));
        assert_eq!(receiver.len(), 1);
        assert!(!drain_output_slice(&receiver, &mut recorder, &mut output));
        drop(output);
        assert_eq!(sink.len(), OUTPUT_SLICE_MESSAGES + 1);
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

        assert!(drain_output_slice(&receiver, &mut recorder, &mut output));
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
        fn ready(&mut self) -> bool {
            panic!("output readiness ran before an imposed lifecycle stop");
        }

        fn start(&mut self, _cols: u16, _rows: u16) {}

        fn output(&mut self, _bytes: Vec<u8>) {
            panic!("output was emitted before an imposed lifecycle stop");
        }

        fn title(&mut self, _title: String) {
            panic!("a title was emitted before an imposed lifecycle stop");
        }

        fn finish(&mut self, code: i32, _duration: Duration, mode: OutputFinish) {
            assert!(matches!(mode, OutputFinish::AbandonPending));
            self.finished = Some(code);
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
        let code = run_exec_engine(opts, &|| None, &mut output, cancelled);
        if code == EXIT_INTERNAL && output.finished.is_none() {
            eprintln!("skipping lifecycle stop ordering test: no PTY");
            return;
        }
        assert_eq!(code, expected);
        assert_eq!(output.finished, Some(expected));
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
            // 0xC0000005 (STATUS_ACCESS_VIOLATION) saturates into i32, not 0x05.
            assert_eq!(clamp_code(0xC000_0005), i32::MAX);
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
        let temp = tempfile::tempdir().unwrap();
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
    fn timeout_kills_a_detached_descendant() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("detached.pid");
        let opts = ExecOpts {
            argv: vec![
                "sh".into(),
                "-c".into(),
                "setsid sleep 30 & echo $! > \"$1\"; wait".into(),
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
            "detached descendant {pid} survived timeout"
        );
    }

    #[cfg(windows)]
    #[test]
    fn timeout_terminates_a_windows_descendant_job() {
        use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{
            OpenProcess, PROCESS_SYNCHRONIZE, WaitForSingleObject,
        };

        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("descendant.pid");
        let escaped_pid_file = pid_file.to_string_lossy().replace('\u{27}', "''");
        let script = format!(
            "$child = Start-Process -FilePath ping.exe -ArgumentList @('-n','30','127.0.0.1') -PassThru; \
             [IO.File]::WriteAllText('{escaped_pid_file}', [string]$child.Id); \
             Wait-Process -Id $child.Id"
        );
        let opts = ExecOpts {
            argv: vec![
                "powershell.exe".into(),
                "-NoLogo".into(),
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                script,
            ],
            cols: 80,
            rows: 24,
            cwd: None,
            // Hosted Windows runners can spend several seconds cold-starting
            // Windows PowerShell after a large workspace build. The fixture
            // must reach WriteAllText before Kettle fires the timeout or the
            // test never observes the descendant it is meant to validate.
            timeout: Some(Duration::from_secs(10)),
            mode: OutputMode::Raw,
            record: None,
            forward_stdin: false,
        };
        let mut sink = Vec::new();

        assert_eq!(run_exec_with(opts, &|| None, &mut sink), EXIT_TIMEOUT);
        let pid: u32 = std::fs::read_to_string(&pid_file)
            .expect("PowerShell must record its descendant pid before timeout")
            .trim()
            .parse()
            .unwrap();
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
        #[cfg(windows)]
        let scratch = std::env::var_os("LOCALAPPDATA")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(std::path::PathBuf::from)
            .expect("Windows tests require LOCALAPPDATA or USERPROFILE");
        #[cfg(not(windows))]
        let scratch = std::env::temp_dir();
        let dir = scratch.join(format!("kettle-exec-cancel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let record = dir.join("cancel.cast");
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
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exec_module_is_headless_no_ui_no_winit() {
        // Drift guard: the headless exec engine must NOT pull in the GUI. If a
        // future edit reaches for a `kettle_ui::` / `winit::` path, this fails —
        // "headless means headless". Scans for the `::` *usage* forms so the
        // module's own doc comment (which names the crates in prose) doesn't
        // trip it; strips this test's body so its assert strings don't either.
        let src = include_str!("exec.rs");
        let scan = src
            .split("fn exec_module_is_headless_no_ui_no_winit")
            .next()
            .unwrap();
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
        o.start(&mut sink, 80, 24);
        let s = String::from_utf8(sink).unwrap();
        assert!(s.contains("\"event\":\"start\""), "got: {s}");
        assert!(s.contains("\"cols\":80"));
    }
}

//! Cycle 922–923 (agent-first A1): `kettle exec` — run a command under a real
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
//!     bytes on a sidechannel. We drive it with a no-op waker and unbounded
//!     channels — the exact shape kettle-core's own headless tests use.
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
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};
use kettle_core::{CursorShape, TermEvent, Terminal, Waker};

/// How long to keep draining output after the child exits before we stop and
/// report the code. Doubles as the ConPTY late-repaint mitigation: ConPTY's
/// screen-differ can emit a final paint after the child is gone. Same order of
/// magnitude as the dev-record reap settle.
const SETTLE: Duration = Duration::from_millis(60);

/// Exit code for `--timeout` expiry (coreutils `timeout(1)` convention).
pub const EXIT_TIMEOUT: i32 = 124;
/// Exit code for an internal kettle error (spawn failure, no PTY, bad args).
pub const EXIT_INTERNAL: i32 = 125;

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

/// A streaming ANSI stripper that is correct across read boundaries.
///
/// `kettle_core::strip_ansi_bytes` is a whole-buffer function that drops a bare
/// trailing `ESC` and can't see a sequence split across two reads. For a live
/// stream we must hold back any incomplete trailing escape sequence until its
/// terminator arrives in the next chunk. This state machine does exactly that:
/// it emits stripped text for every *complete* run and carries an in-progress
/// escape across `push` calls.
#[derive(Default)]
pub struct AnsiStripper {
    /// Bytes of an escape sequence seen so far that has not yet terminated.
    /// Empty when we are not inside a sequence.
    pending: Vec<u8>,
}

impl AnsiStripper {
    /// Feed a chunk; append the stripped plaintext to `out`.
    pub fn push(&mut self, input: &[u8], out: &mut Vec<u8>) {
        for &b in input {
            if self.pending.is_empty() {
                if b == 0x1b {
                    self.pending.push(b);
                } else {
                    out.push(b);
                }
                continue;
            }
            // Inside an escape sequence: accumulate and test for termination.
            self.pending.push(b);
            if self.sequence_complete() {
                self.pending.clear();
            }
        }
    }

    /// Has `self.pending` reached a complete escape sequence?
    fn sequence_complete(&self) -> bool {
        let p = &self.pending;
        // Need at least ESC + introducer to classify.
        if p.len() < 2 {
            return false;
        }
        match p[1] {
            // CSI: ESC [ params… final(0x40..=0x7e)
            b'[' => p.len() >= 3 && (0x40..=0x7e).contains(&p[p.len() - 1]),
            // OSC: ESC ] … (BEL | ESC \)
            b']' => {
                let last = p[p.len() - 1];
                last == 0x07 || (p.len() >= 4 && p[p.len() - 2] == 0x1b && last == b'\\')
            }
            // Any other single-char ESC X — complete at 2 bytes.
            _ => true,
        }
    }
}

/// Run `kettle exec` end to end; returns the process exit code to propagate.
pub fn run_exec(opts: ExecOpts) -> i32 {
    run_exec_with(opts, &default_size_probe, &mut std::io::stdout().lock())
}

/// Cycle 932 (agent-first A3): run a command headlessly and CAPTURE its output
/// in-process (instead of streaming to stdout) — the engine behind the
/// `kettle_run` MCP tool. Returns `(exit_code, output)`; the output is the
/// tail-capped (1 MiB) child output in the requested mode (strip-ansi
/// recommended for agent assertions).
pub fn run_exec_capture(opts: ExecOpts) -> (i32, String) {
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
    let code = run_exec_with(opts, &default_size_probe, &mut sink);
    (code, String::from_utf8_lossy(&sink.buf).into_owned())
}

/// Default console-size probe (real terminal dimensions when stdout is a TTY).
pub fn default_size_probe() -> Option<(u16, u16)> {
    terminal_size_cols_rows()
}

/// Core run loop, with the stdout sink and size probe injected for testing.
pub fn run_exec_with(
    mut opts: ExecOpts,
    _size_probe: &dyn Fn() -> Option<(u16, u16)>,
    sink: &mut dyn Write,
) -> i32 {
    if opts.argv.is_empty() {
        let _ = writeln!(std::io::stderr(), "kettle exec: no command given");
        return EXIT_INTERNAL;
    }
    // Clamp geometry into a sane PTY range (kettle-core clamps too, but a 0
    // here would make ConPTY unhappy).
    opts.cols = opts.cols.clamp(1, u16::MAX);
    opts.rows = opts.rows.clamp(1, u16::MAX);

    let (tx, rx): (Sender<TermEvent>, Receiver<TermEvent>) = crossbeam_channel::unbounded();
    let (otx, orx): (Sender<Vec<u8>>, Receiver<Vec<u8>>) = crossbeam_channel::unbounded();
    let waker: Waker = std::sync::Arc::new(|| {});
    let cwd = opts.cwd.as_ref().and_then(|p| p.to_str());

    let term = match Terminal::new_with_env_and_output(
        &opts.argv,
        cwd,
        // Modest scrollback — exec output streams out immediately, the grid is
        // only used for VT state + query answers.
        2000,
        opts.cols as usize,
        opts.rows as usize,
        8,
        16,
        false,
        CursorShape::Block,
        None,
        "xterm-256color",
        "truecolor",
        false,
        tx,
        waker,
        Some(otx),
    ) {
        Ok(t) => t,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "kettle exec: cannot start PTY: {e}");
            return EXIT_INTERNAL;
        }
    };

    // Optional stdin → PTY pump (only for pipe/file/socket stdin).
    // The pump holds a cloneable `PtyWriter`, so the (non-`Sync`) `Terminal`
    // stays owned by this loop.
    if opts.forward_stdin {
        spawn_stdin_pump(term.writer_handle());
    }

    // Optional asciicast (.cast) recording (output-only — exec never routes
    // keystrokes here; the audit trail is verbatim child output + resize).
    let mut recorder =
        opts.record.as_ref().and_then(|p| {
            match kettle_core::record::Recorder::start(p, opts.cols, opts.rows, false) {
                Ok(r) => Some(r),
                Err(e) => {
                    let _ = writeln!(std::io::stderr(), "kettle exec: --record failed: {e}");
                    None
                }
            }
        });

    let mut out = Outputter::new(opts.mode);
    out.start(sink, opts.cols, opts.rows);

    let started = Instant::now();
    let mut child_gone_at: Option<Instant> = None;

    loop {
        // Drain output first so we never lose bytes that arrived before exit.
        while let Ok(bytes) = orx.try_recv() {
            if let Some(r) = recorder.as_mut() {
                r.record_output(&bytes);
            }
            out.output(sink, &bytes);
        }
        // Service the child's terminal queries + lifecycle events.
        while let Ok(ev) = rx.try_recv() {
            match ev {
                TermEvent::PtyWrite(s) => term.write(s.as_bytes()),
                TermEvent::Title(t) => out.title(sink, &t),
                TermEvent::TextAreaSizeRequest(fmt) => {
                    let reply =
                        kettle_render::reply_for_text_area_size(opts.cols, opts.rows, 8, 16, &*fmt);
                    term.write(reply.as_bytes());
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
                        term.write(s.as_bytes());
                    }
                }
                // OSC 52 read: deny (reply empty so the protocol stays
                // well-formed without leaking a clipboard to a headless child).
                TermEvent::ClipboardLoad(_, fmt) => term.write(fmt("").as_bytes()),
                TermEvent::Exit | TermEvent::ChildExit(_) => {
                    child_gone_at.get_or_insert_with(Instant::now);
                }
                _ => {}
            }
        }

        // Exit detection: poll the real child status (authoritative), then
        // settle-drain so trailing/late output (ConPTY repaint) is captured.
        if child_gone_at.is_none() && term.child_exited() {
            child_gone_at = Some(Instant::now());
        }
        if let Some(gone) = child_gone_at
            && gone.elapsed() >= SETTLE
            && orx.is_empty()
        {
            // Final drain in case something landed in the settle window.
            while let Ok(bytes) = orx.try_recv() {
                if let Some(r) = recorder.as_mut() {
                    r.record_output(&bytes);
                }
                out.output(sink, &bytes);
            }
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
            out.finish(sink, code, started.elapsed());
            return code;
        }

        // Timeout: kill the child, settle briefly, report 124.
        if let Some(limit) = opts.timeout
            && started.elapsed() >= limit
            && child_gone_at.is_none()
        {
            term.kill();
            std::thread::sleep(SETTLE);
            while let Ok(bytes) = orx.try_recv() {
                if let Some(r) = recorder.as_mut() {
                    r.record_output(&bytes);
                }
                out.output(sink, &bytes);
            }
            if let Some(mut r) = recorder.take() {
                r.finish();
            }
            out.finish(sink, EXIT_TIMEOUT, started.elapsed());
            return EXIT_TIMEOUT;
        }

        std::thread::sleep(Duration::from_millis(8));
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
            let v = serde_json::json!({
                "v":1,"event":"exit","code":code,"duration_ms":dur.as_millis() as u64
            });
            let _ = writeln!(sink, "{v}");
        }
        let _ = sink.flush();
    }
}

/// Spawn a thread that pumps this process's stdin into the PTY in 8 KiB
/// chunks, stopping on EOF. On EOF it closes the PTY's input side so a
/// read-until-EOF child (`cat`, `sort`, `wc`) sees end-of-input and exits —
/// the correct PTY EOF mechanism (an injected Ctrl+D/Ctrl+Z byte does NOT work
/// under Windows ConPTY). Closing input does not kill the child.
fn spawn_stdin_pump(writer: kettle_core::PtyWriter) {
    std::thread::spawn(move || {
        use std::io::Read;
        let mut stdin = std::io::stdin().lock();
        let mut buf = [0u8; 8192];
        loop {
            match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => writer.write(&buf[..n]),
            }
        }
        // Windows ConPTY does not turn a conin pipe-close into EOF for
        // ReadConsole-based children (`sort`, `more`); the console EOF is
        // Ctrl+Z at the start of a line followed by Enter. Send that nudge
        // first, THEN close the pipe (covers both line-buffered console
        // readers and raw pipe readers). Unix PTYs EOF cleanly on close alone.
        #[cfg(windows)]
        writer.write(b"\x1a\r\n");
        writer.close();
    });
}

/// True when stdin is a pipe/file/socket we should forward to the PTY
/// (`echo y | kettle exec -- …`). On an interactive TTY we do NOT steal stdin
/// (the human is pointed at the GUI), and `/dev/null`/`NUL` stays closed rather
/// than being treated as useful input.
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
        windows_stdin_is_pipe()
    }
    #[cfg(not(any(unix, windows)))]
    {
        false
    }
}

#[cfg(windows)]
#[allow(dead_code)]
fn windows_stdin_is_pipe() -> bool {
    use windows_sys::Win32::Storage::FileSystem::{FILE_TYPE_CHAR, GetFileType};
    use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE};
    // A console device is FILE_TYPE_CHAR; a pipe/file is something else.
    unsafe {
        let h = GetStdHandle(STD_INPUT_HANDLE);
        if h.is_null() {
            return false;
        }
        GetFileType(h) != FILE_TYPE_CHAR
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

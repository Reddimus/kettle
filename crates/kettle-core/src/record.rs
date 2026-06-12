//! Asciicast v2 session recorder (cargo feature `asciicast`).
//!
//! Cycle 875 introduced this as kettle-ui's developer-only `dev-record`
//! recorder; cycle 924 (agent-first A1) promoted it here to kettle-core — the
//! crate that owns the `Terminal` — so it is the ONE shared recorder behind
//! both the GUI's `--record` (kettle-ui `dev-record` feature) and `kettle exec
//! --record` (the bin enables `kettle-core/asciicast` unconditionally, so
//! recording an agent run ships in release builds; that path is output-only —
//! no keystroke-privacy surface).
//!
//! Writes an asciicast v2-compatible NDJSON trace that replays in
//! `asciinema play`:
//!
//! - line 1: a `{"version":2,"width":W,"height":H,...}` header
//! - `[t, "o", <utf8>]`   — terminal OUTPUT
//! - `[t, "r", "CxR"]`    — resize
//! - `[t, "m", <json>]`   — kettle UI/UX markers (players ignore them)
//! - `[t, "i", <token>]`  — keystroke TOKENS, never raw typed chars (cycle 876)
//!
//! The file is created `0600` on Unix and is purely local — kettle never
//! uploads it. Writes are best-effort: the first I/O error disables the
//! recorder (a full disk must never crash the terminal).
//!
//! Privacy: terminal OUTPUT is VERBATIM and cannot be redacted — a terminal
//! can't tell a secret from normal output, so anything printed/echoed on
//! screen lands in cleartext. Review/scrub a `.cast` before sharing it (see
//! docs/DEV-RECORD.md).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::{Duration, Instant};

/// v2.20.0 P5 (perf): how often buffered events are flushed to disk. Events
/// between flushes sit in the `BufWriter` (which also self-flushes whenever
/// its 8KiB buffer fills, so a flood can't grow the loss window); a hard
/// crash loses at most this much trailing trace. `finish` / `Drop` still
/// flush, so every clean close path produces a complete, replayable file —
/// the cycle-908 closure verification is unaffected.
const FLUSH_INTERVAL: Duration = Duration::from_millis(250);

/// An append-only asciicast writer. One per recording session.
pub struct Recorder {
    writer: BufWriter<File>,
    start: Instant,
    /// Set after the first write error so we stop trying (fail-silent).
    disabled: bool,
    /// Cycle 876: when true, record raw typed characters in `i` events.
    /// Default false — bare printables are redacted to a generic class so a
    /// typed password never lands in the trace (`--record-raw-input` opts in).
    raw_input: bool,
    /// Trailing bytes of an INCOMPLETE multibyte UTF-8 sequence carried over to
    /// the next `record_output` chunk, so a codepoint split across two PTY reads
    /// is decoded whole instead of being mangled into U+FFFD on each side.
    utf8_carry: Vec<u8>,
    /// v2.20.0 P5: when the buffer was last explicitly flushed (see
    /// [`FLUSH_INTERVAL`]).
    last_flush: Instant,
    /// v2.20.0 (review fix): lines written since the last flush. Without
    /// this, a burst followed by silence left the tail buffered FOREVER
    /// (the interval flush is event-driven) — `flush_if_stale` lets the
    /// app's timer loop bound staleness to ~FLUSH_INTERVAL in wall time.
    dirty: bool,
}

impl Recorder {
    /// Open `path` (truncating), write the asciicast header sized to the current
    /// grid, and start the monotonic clock. Errors propagate so the caller can
    /// log + skip recording without affecting the terminal.
    pub fn start(path: &Path, cols: u16, rows: u16, raw_input: bool) -> std::io::Result<Self> {
        let file = open_private(path)?;
        let mut writer = BufWriter::new(file);
        writeln!(writer, "{}", header_line(cols, rows))?;
        writer.flush()?;
        Ok(Self {
            writer,
            start: Instant::now(),
            disabled: false,
            raw_input,
            utf8_carry: Vec::new(),
            last_flush: Instant::now(),
            dirty: false,
        })
    }

    /// Whether raw typed characters are captured (vs redacted). Cycle 876.
    pub fn raw_input(&self) -> bool {
        self.raw_input
    }

    fn emit(&mut self, code: &str, data: &str) {
        if self.disabled {
            return;
        }
        let secs = self.start.elapsed().as_secs_f64();
        let line = event_line(secs, code, data);
        // v2.20.0 P5: flush on a ~250ms interval instead of per event. The
        // old per-event flush put a write syscall on the UI thread for every
        // PTY read under flood — and the installed dev-record build records
        // EVERY session. Crash exposure is bounded by FLUSH_INTERVAL;
        // `finish`/`Drop` keep the clean-close trace complete.
        let flush_due = self.last_flush.elapsed() >= FLUSH_INTERVAL;
        let result = writeln!(self.writer, "{line}").and_then(|()| {
            if flush_due {
                self.last_flush = Instant::now();
                self.dirty = false;
                self.writer.flush()
            } else {
                self.dirty = true;
                Ok(())
            }
        });
        if result.is_err() {
            log::warn!("record: write failed; disabling the recorder");
            self.disabled = true;
        }
    }

    /// v2.20.0 (review fix): flush buffered events if any have been sitting
    /// unflushed past `FLUSH_INTERVAL` (250ms). The interval flush in `emit` is
    /// EVENT-driven — a burst followed by silence would otherwise leave its
    /// tail buffered until the next event or a clean close. The app's timer
    /// loop calls this (see `flush_deadline`) to bound the staleness in
    /// wall-clock time.
    pub fn flush_if_stale(&mut self) {
        if self.disabled || !self.dirty || self.last_flush.elapsed() < FLUSH_INTERVAL {
            return;
        }
        self.last_flush = Instant::now();
        self.dirty = false;
        if self.writer.flush().is_err() {
            log::warn!("record: flush failed; disabling the recorder");
            self.disabled = true;
        }
    }

    /// When `flush_if_stale` next needs to run, or `None` when nothing is
    /// buffered. Lets the caller schedule a precise wake instead of polling.
    pub fn flush_deadline(&self) -> Option<Instant> {
        (!self.disabled && self.dirty).then(|| self.last_flush + FLUSH_INTERVAL)
    }

    /// Record a chunk of terminal OUTPUT (`o`). A multibyte codepoint split
    /// across two PTY reads is carried over and decoded whole (not mangled into
    /// U+FFFD on each side); genuinely-invalid bytes still become U+FFFD so the
    /// trace stays valid asciicast / valid JSON.
    ///
    /// Privacy: this is VERBATIM and cannot be redacted — a terminal can't tell
    /// a secret from normal output, so anything printed/echoed on screen lands
    /// in the trace in cleartext. Review/scrub a `.cast` before sharing it (see
    /// docs/DEV-RECORD.md).
    pub fn record_output(&mut self, bytes: &[u8]) {
        if self.disabled {
            return;
        }
        self.utf8_carry.extend_from_slice(bytes);
        let mut out = String::new();
        // Decode as much valid UTF-8 as possible; loop so a chunk that contains
        // [valid][invalid][valid] emits all of it, retaining only a genuinely-
        // incomplete trailing sequence for the next call.
        loop {
            match std::str::from_utf8(&self.utf8_carry) {
                Ok(s) => {
                    out.push_str(s);
                    self.utf8_carry.clear();
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    // SAFETY: bytes up to `valid` are guaranteed valid UTF-8.
                    out.push_str(unsafe {
                        std::str::from_utf8_unchecked(&self.utf8_carry[..valid])
                    });
                    match e.error_len() {
                        // Incomplete trailing sequence — keep it for the next chunk.
                        None => {
                            self.utf8_carry.drain(..valid);
                            break;
                        }
                        // A genuinely-invalid run — emit one replacement, drop it,
                        // and continue decoding the remainder.
                        Some(n) => {
                            out.push('\u{FFFD}');
                            self.utf8_carry.drain(..valid + n);
                        }
                    }
                }
            }
        }
        if !out.is_empty() {
            self.emit("o", &out);
        }
    }

    /// Record a grid resize (`r`), data `"<cols>x<rows>"`.
    pub fn record_resize(&mut self, cols: u16, rows: u16) {
        self.emit("r", &format!("{cols}x{rows}"));
    }

    /// Record a keystroke as an `i` event. Cycle 876: the caller passes a
    /// privacy-preserving TOKEN (a named key / chord like `Enter` / `Ctrl+c`,
    /// or a redacted printable class via `printable_token`) — never raw typed
    /// characters unless raw-input mode was opted into. Pasted content is never
    /// routed here (it's a `paste` marker instead).
    pub fn record_input(&mut self, token: &str) {
        self.emit("i", token);
    }

    /// Record a kettle UI/UX state transition as an `m` marker (cycle 876).
    /// `label` is a short tag like `kettle:tab_add` / `kettle:focus_out` /
    /// `kettle:agent send_text pane=3`. Players that understand markers show
    /// the label; others ignore it. Captures state the PTY output stream can't
    /// (kettle's own tab bar / overlays / focus / agent control), incl.
    /// non-interactive transitions.
    pub fn record_marker(&mut self, label: &str) {
        self.emit("m", label);
    }

    /// Flush any buffered events. Called on close and from `Drop`. Emits any
    /// trailing carried-over bytes (a genuinely-truncated final UTF-8 sequence)
    /// as a U+FFFD so no output is silently dropped at end-of-stream.
    pub fn finish(&mut self) {
        if !self.utf8_carry.is_empty() && !self.disabled {
            let tail = String::from_utf8_lossy(&self.utf8_carry).into_owned();
            self.utf8_carry.clear();
            self.emit("o", &tail);
        }
        self.dirty = false;
        let _ = self.writer.flush();
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Open `path` for truncating writes, `0600` on Unix (local-only secrecy).
fn open_private(path: &Path) -> std::io::Result<File> {
    let mut opts = std::fs::OpenOptions::new();
    opts.create(true).write(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    opts.open(path)
}

/// The asciicast v2 header line for a `cols`×`rows` grid. Pure (unit-tested).
fn header_line(cols: u16, rows: u16) -> String {
    format!(
        "{{\"version\":2,\"width\":{cols},\"height\":{rows},\"env\":{{\"TERM\":\"xterm-256color\",\"KETTLE\":\"{}\"}}}}",
        env!("CARGO_PKG_VERSION")
    )
}

/// One asciicast event line `[time, "code", "data"]` with `data` JSON-escaped
/// (control bytes / quotes / newlines handled by `serde_json`). Pure
/// (unit-tested) so the format is verifiable without a file.
fn event_line(time: f64, code: &str, data: &str) -> String {
    let data_json = serde_json::to_string(data).unwrap_or_else(|_| "\"\"".to_string());
    format!("[{time:.6}, \"{code}\", {data_json}]")
}

/// Cycle 876: redact a bare printable keystroke. In raw mode the literal text is
/// kept (full-fidelity repro the dev explicitly opted into with
/// `--record-raw-input`); otherwise each character collapses to a generic class
/// glyph so a typed password never appears in the trace — only its keystroke
/// count and timing survive. Pure (unit-tested).
pub fn printable_token(text: &str, raw: bool) -> String {
    if raw {
        text.to_string()
    } else {
        "·".repeat(text.chars().count().max(1))
    }
}

#[cfg(test)]
mod tests {
    use super::{event_line, header_line, printable_token};

    #[test]
    fn header_is_valid_asciicast_v2_json() {
        let h = header_line(120, 40);
        let v: serde_json::Value = serde_json::from_str(&h).expect("header must be valid JSON");
        assert_eq!(v["version"], 2);
        assert_eq!(v["width"], 120);
        assert_eq!(v["height"], 40);
    }

    #[test]
    fn event_line_is_valid_json_and_escapes_control_bytes() {
        // Output containing a quote, a newline and an ESC must round-trip as a
        // single valid JSON array (no literal newline breaking the NDJSON line).
        let line = event_line(1.5, "o", "he\"llo\n\x1b[0m");
        assert!(
            !line[1..].contains('\n'),
            "control newline must be escaped, not literal: {line}"
        );
        let v: serde_json::Value = serde_json::from_str(&line).expect("event must be valid JSON");
        assert_eq!(v[0], 1.5);
        assert_eq!(v[1], "o");
        assert_eq!(v[2], "he\"llo\n\x1b[0m");
    }

    #[test]
    fn event_time_has_microsecond_precision() {
        let line = event_line(0.123456, "o", "x");
        assert!(line.starts_with("[0.123456, \"o\","), "{line}");
    }

    #[test]
    fn printable_token_redacts_unless_raw() {
        // Default: each char collapses to a class glyph — count/timing survive,
        // the secret content does not.
        assert_eq!(printable_token("p", false), "·");
        assert_eq!(printable_token("abc", false), "···");
        assert!(
            !printable_token("hunter2", false).contains('h'),
            "redacted token must not leak the typed characters"
        );
        // Raw opt-in: literal characters are kept.
        assert_eq!(printable_token("abc", true), "abc");
    }

    /// Cycle 936 (review): a multibyte codepoint split across two
    /// `record_output` chunks must decode whole, not mangle into U+FFFD halves.
    #[test]
    fn record_output_stitches_split_utf8_across_chunks() {
        use std::io::Read;
        let path =
            std::env::temp_dir().join(format!("kettle-rec-utf8-{}.cast", std::process::id()));
        {
            let mut rec = super::Recorder::start(&path, 80, 24, false).expect("start");
            // "é" = 0xC3 0xA9; "中" = 0xE4 0xB8 0xAD. Split each across chunks.
            rec.record_output(&[b'a', 0xC3]); // 'a' + first byte of 'é'
            rec.record_output(&[0xA9, 0xE4, 0xB8]); // rest of 'é' + first 2 of '中'
            rec.record_output(&[0xAD, b'b']); // last of '中' + 'b'
            rec.finish();
        }
        let mut s = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        // Collect all `o` event payloads, concatenated.
        let joined: String = s
            .lines()
            .skip(1)
            .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
            .filter(|v| v[1] == "o")
            .filter_map(|v| v[2].as_str().map(String::from))
            .collect();
        assert_eq!(
            joined, "aé中b",
            "split multibyte codepoints must reassemble whole"
        );
        assert!(
            !joined.contains('\u{FFFD}'),
            "no replacement chars: {joined:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn writes_a_replayable_asciicast_file() {
        use std::io::Read;
        // Unique temp path (pid, not random/clock — those are unavailable here).
        let path = std::env::temp_dir().join(format!("kettle-rec-{}.cast", std::process::id()));
        {
            let mut rec = super::Recorder::start(&path, 80, 24, false).expect("start recorder");
            rec.record_output(b"hello\r\n");
            rec.record_resize(100, 30);
            rec.record_output(b"\x1b[31mred\x1b[0m");
            rec.finish();
        }
        let mut s = String::new();
        std::fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut s)
            .unwrap();
        let mut lines = s.lines();
        let header: serde_json::Value = serde_json::from_str(lines.next().unwrap()).unwrap();
        assert_eq!(header["version"], 2);
        assert_eq!(header["width"], 80);
        assert_eq!(header["height"], 24);
        let events: Vec<serde_json::Value> = lines
            .map(|l| serde_json::from_str(l).expect("each event is valid JSON"))
            .collect();
        assert!(
            events.iter().any(|e| e[1] == "o" && e[2] == "hello\r\n"),
            "output event missing"
        );
        assert!(
            events.iter().any(|e| e[1] == "r" && e[2] == "100x30"),
            "resize event missing"
        );
        let _ = std::fs::remove_file(&path);
    }
}

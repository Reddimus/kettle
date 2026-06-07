//! Cycle 875: developer-only session recorder (Cargo feature `dev-record`,
//! compiled OUT of released / packaged builds). Writes an asciicast v2-compatible
//! NDJSON trace that replays in `asciinema play`:
//!
//! - line 1: a `{"version":2,"width":W,"height":H,...}` header
//! - `[t, "o", <utf8>]`   — terminal OUTPUT
//! - `[t, "r", "CxR"]`    — resize
//! - `[t, "m", <json>]`   — kettle UI/UX markers (cycle 876; players ignore them)
//! - `[t, "i", <token>]`  — keystroke TOKENS, never raw typed chars (cycle 876)
//!
//! Activated only via `kettle --record <path>` (or `KETTLE_RECORD`); never on by
//! default, never on first launch. The file is created `0600` on Unix and is
//! purely local — kettle never uploads it. Writes are best-effort: the first I/O
//! error disables the recorder (a full disk must never crash the terminal).

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::Path;
use std::time::Instant;

/// An append-only asciicast writer. One per recording session, owned by `App`.
pub struct Recorder {
    writer: BufWriter<File>,
    start: Instant,
    /// Set after the first write error so we stop trying (fail-silent).
    disabled: bool,
}

impl Recorder {
    /// Open `path` (truncating), write the asciicast header sized to the current
    /// grid, and start the monotonic clock. Errors propagate so the caller can
    /// log + skip recording without affecting the terminal.
    pub fn start(path: &Path, cols: u16, rows: u16) -> std::io::Result<Self> {
        let file = open_private(path)?;
        let mut writer = BufWriter::new(file);
        writeln!(writer, "{}", header_line(cols, rows))?;
        writer.flush()?;
        Ok(Self {
            writer,
            start: Instant::now(),
            disabled: false,
        })
    }

    fn emit(&mut self, code: &str, data: &str) {
        if self.disabled {
            return;
        }
        let secs = self.start.elapsed().as_secs_f64();
        let line = event_line(secs, code, data);
        // Flush each event so a crash mid-session still leaves a usable trace.
        if writeln!(self.writer, "{line}")
            .and_then(|()| self.writer.flush())
            .is_err()
        {
            log::warn!("dev-record: write failed; disabling the recorder");
            self.disabled = true;
        }
    }

    /// Record a chunk of terminal OUTPUT (`o`). Non-UTF-8 bytes become U+FFFD —
    /// the trace stays valid asciicast / valid JSON, not byte-perfect.
    pub fn record_output(&mut self, bytes: &[u8]) {
        let text = String::from_utf8_lossy(bytes);
        self.emit("o", &text);
    }

    /// Record a grid resize (`r`), data `"<cols>x<rows>"`.
    pub fn record_resize(&mut self, cols: u16, rows: u16) {
        self.emit("r", &format!("{cols}x{rows}"));
    }

    /// Flush any buffered events. Called on close and from `Drop`.
    pub fn finish(&mut self) {
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

#[cfg(test)]
mod tests {
    use super::{event_line, header_line};

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
    fn writes_a_replayable_asciicast_file() {
        use std::io::Read;
        // Unique temp path (pid, not random/clock — those are unavailable here).
        let path = std::env::temp_dir().join(format!("kettle-devrec-{}.cast", std::process::id()));
        {
            let mut rec = super::Recorder::start(&path, 80, 24).expect("start recorder");
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

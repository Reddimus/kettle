//! Cycle 327: tmux control-mode (`tmux -CC`) protocol parser.
//!
//! When tmux runs with the `-CC` flag, it switches to a control-mode
//! that's quite different from normal terminal output:
//!
//! - Every multi-line response is wrapped in
//!   `%begin TIME N FLAGS\n...lines...\n%end TIME N FLAGS\n`.
//! - Async notifications (windows added, panes resized, output
//!   appearing) come as single `%name args\n` lines outside any
//!   `%begin/%end` block.
//! - The TIME is a Unix timestamp; N is a per-message counter; FLAGS
//!   is currently always 0 or 1.
//!
//! kettle's integration is multi-stage:
//!
//!   cycle 327 (this one): pure-parser foundation. Feed it byte
//!     chunks via `feed(&[u8])`; consume one or more `TmuxEvent`
//!     via the returned iterator. No App integration yet.
//!   cycle 328+: pane-level state — a Pane can be "in tmux control
//!     mode", which routes its PTY output through this parser.
//!   cycle 329+: surface tmux windows as kettle tabs (one tab per
//!     tmux window inside the controlled session).
//!   cycle 330+: route user keystrokes back to tmux via the
//!     control channel's `send-keys` command.
//!
//! Reference: <https://github.com/tmux/tmux/blob/master/control.c>
//! iTerm2's tmux integration: <https://iterm2.com/documentation-tmux-integration.html>

use std::collections::VecDeque;

/// Output of one feed step. Each variant maps 1:1 to a tmux control-
/// mode message; the data is parsed into structured fields where
/// cheap to do so + left as raw bytes where layout-specific.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TmuxEvent {
    /// `%begin TIME N FLAGS` — start of a multi-line response.
    /// Lines that follow (until `%end`) accumulate as `Output`.
    Begin { time: u64, seq: u64, flags: u32 },
    /// `%end TIME N FLAGS` — close of the most recent `%begin`.
    End { time: u64, seq: u64, flags: u32 },
    /// `%error TIME N FLAGS` — close of the most recent `%begin`,
    /// but with a non-zero exit-status semantic.
    Error { time: u64, seq: u64, flags: u32 },
    /// `%output %ID DATA` — terminal output from the named pane.
    /// `pane_id` is the integer after the `%` (tmux's per-pane id).
    /// `data` is decoded with `\<octal>` escapes already expanded.
    Output { pane_id: u32, data: Vec<u8> },
    /// `%window-add @ID` — a new window appeared. ID is tmux's
    /// per-window id (always positive).
    WindowAdd { window_id: u32 },
    /// `%window-close @ID` — window gone.
    WindowClose { window_id: u32 },
    /// `%window-renamed @ID NAME`.
    WindowRenamed { window_id: u32, name: String },
    /// `%session-changed $ID NAME`.
    SessionChanged { session_id: u32, name: String },
    /// `%session-renamed NAME`.
    SessionRenamed { name: String },
    /// `%layout-change @ID LAYOUT`. Layout string is tmux's compact
    /// format (parsing is future cycle's job).
    LayoutChange { window_id: u32, layout: String },
    /// `%client-detached CLIENT` — a client (possibly us) detached.
    ClientDetached { client: String },
    /// `%exit [REASON]` — control session ending. None when tmux
    /// closes cleanly without a reason string.
    Exit { reason: Option<String> },
    /// A line we recognized as a `%...` event but didn't match any
    /// specific arm. Caller can decide to log or ignore.
    Unknown { line: String },
    /// A raw output line that arrived OUTSIDE any `%begin/%end`
    /// block — shouldn't happen in well-behaved tmux but is
    /// emitted for safety so a malformed stream doesn't lose data.
    OutsideBlock { line: String },
}

/// Streaming parser. Holds an internal byte buffer; feed it raw
/// PTY bytes; pull `TmuxEvent`s out. Cheap to construct (just
/// allocates the buffer); reusing one across a long-lived session
/// avoids re-allocation.
#[derive(Default)]
pub struct TmuxControlParser {
    /// Bytes received but not yet forming a complete `\n`-terminated
    /// line. Grows up to MAX_LINE before being dropped (defense
    /// against a corrupt or hostile stream).
    buf: Vec<u8>,
    /// Events ready to be popped by the caller. Each `feed` call
    /// can produce zero, one, or many.
    events: VecDeque<TmuxEvent>,
}

/// Cap on the in-progress line buffer. Realistic tmux control lines
/// max out a few KB (output frames after escape decoding). Anything
/// larger is almost certainly a corrupt stream; drop it.
const MAX_LINE: usize = 64 * 1024;

impl TmuxControlParser {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed a chunk of bytes. Splits at `\n` boundaries; partial
    /// lines stay in the internal buffer until the next feed. Pulls
    /// events into the queue as complete lines arrive.
    pub fn feed(&mut self, chunk: &[u8]) {
        for &byte in chunk {
            if byte == b'\n' {
                let line_bytes = std::mem::take(&mut self.buf);
                // Lossy UTF-8 is fine — tmux control lines are
                // ASCII except inside %output's data which we
                // decode separately (and back to raw bytes).
                let line = String::from_utf8_lossy(&line_bytes).into_owned();
                if let Some(ev) = parse_line(&line) {
                    self.events.push_back(ev);
                }
                continue;
            }
            if self.buf.len() >= MAX_LINE {
                // Overflow guard: drop the malformed buffer + keep
                // scanning. Reset state so we don't stall the
                // parser forever on a single huge line.
                self.buf.clear();
            }
            self.buf.push(byte);
        }
    }

    /// Pop one event; returns None when the queue is empty.
    pub fn next_event(&mut self) -> Option<TmuxEvent> {
        self.events.pop_front()
    }

    /// Drain every pending event in order. Convenience for callers
    /// who want a Vec.
    pub fn drain(&mut self) -> Vec<TmuxEvent> {
        std::mem::take(&mut self.events).into_iter().collect()
    }
}

/// Decode one line of tmux control output. Returns `None` for lines
/// that don't start with `%` — the caller can either ignore those or
/// surface them as `OutsideBlock` (we surface, since silently
/// dropping is the kind of bug that's a pain to diagnose later).
fn parse_line(line: &str) -> Option<TmuxEvent> {
    // Strip a trailing CR if present (some pty layers add CRLF).
    let line = line.trim_end_matches('\r');
    if !line.starts_with('%') {
        // tmux puts non-% lines inside %begin/%end blocks. We don't
        // currently track block state, so surface them so callers
        // can choose to attach to the most recent %begin response.
        return Some(TmuxEvent::OutsideBlock {
            line: line.to_string(),
        });
    }
    // Strip the leading %.
    let body = &line[1..];
    // Split off the verb (e.g. "output %1 some-data" → ("output", "%1 some-data")).
    let (verb, rest) = match body.split_once(' ') {
        Some((v, r)) => (v, r),
        None => (body, ""),
    };
    Some(match verb {
        "begin" => parse_begin_end(rest, |t, s, f| TmuxEvent::Begin {
            time: t,
            seq: s,
            flags: f,
        })?,
        "end" => parse_begin_end(rest, |t, s, f| TmuxEvent::End {
            time: t,
            seq: s,
            flags: f,
        })?,
        "error" => parse_begin_end(rest, |t, s, f| TmuxEvent::Error {
            time: t,
            seq: s,
            flags: f,
        })?,
        "output" => parse_output(rest)?,
        "window-add" => TmuxEvent::WindowAdd {
            window_id: parse_at_id(rest)?,
        },
        "window-close" => TmuxEvent::WindowClose {
            window_id: parse_at_id(rest)?,
        },
        "window-renamed" => {
            let (id_part, name) = rest.split_once(' ')?;
            TmuxEvent::WindowRenamed {
                window_id: parse_at_id(id_part)?,
                name: name.to_string(),
            }
        }
        "session-changed" => {
            let (id_part, name) = rest.split_once(' ')?;
            TmuxEvent::SessionChanged {
                session_id: parse_dollar_id(id_part)?,
                name: name.to_string(),
            }
        }
        "session-renamed" => TmuxEvent::SessionRenamed {
            name: rest.to_string(),
        },
        "layout-change" => {
            let (id_part, layout) = rest.split_once(' ')?;
            TmuxEvent::LayoutChange {
                window_id: parse_at_id(id_part)?,
                layout: layout.to_string(),
            }
        }
        "client-detached" => TmuxEvent::ClientDetached {
            client: rest.to_string(),
        },
        "exit" => TmuxEvent::Exit {
            reason: if rest.is_empty() {
                None
            } else {
                Some(rest.to_string())
            },
        },
        _ => TmuxEvent::Unknown {
            line: line.to_string(),
        },
    })
}

/// `TIME N FLAGS` → (time, seq, flags). Returns None when the format
/// is wrong (which shouldn't happen with real tmux but defends
/// against a malformed stream).
fn parse_begin_end<F>(rest: &str, ctor: F) -> Option<TmuxEvent>
where
    F: FnOnce(u64, u64, u32) -> TmuxEvent,
{
    let mut parts = rest.split_whitespace();
    let time = parts.next()?.parse().ok()?;
    let seq = parts.next()?.parse().ok()?;
    let flags = parts.next()?.parse().ok()?;
    Some(ctor(time, seq, flags))
}

/// `%ID DATA` → Output. tmux escapes ASCII control chars as
/// `\nnn` (3-digit octal) in `%output`; decode those back to raw
/// bytes so the receiver can write to the terminal.
fn parse_output(rest: &str) -> Option<TmuxEvent> {
    let (id_part, data) = rest.split_once(' ')?;
    let pane_id = parse_pct_id(id_part)?;
    let mut bytes = Vec::with_capacity(data.len());
    let mut chars = data.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            // Expect 3 octal digits.
            let d1 = chars.next()?.to_digit(8)?;
            let d2 = chars.next()?.to_digit(8)?;
            let d3 = chars.next()?.to_digit(8)?;
            // Cycle 858 (audit): octal 400..777 (= 256..511) exceeds a byte.
            // tmux only emits 000..377 for real bytes, so a larger value is
            // malformed — reject the event rather than silently truncating it
            // to a wrong byte with `as u8`.
            let byte = u8::try_from(d1 * 64 + d2 * 8 + d3).ok()?;
            bytes.push(byte);
        } else {
            // tmux emits ASCII directly; UTF-8 multibyte glyphs in
            // pane content are encoded as raw bytes inside `\nnn`
            // sequences, not as UTF-8 codepoints, so this branch
            // sees only single-byte ASCII characters.
            bytes.push(c as u8);
        }
    }
    Some(TmuxEvent::Output {
        pane_id,
        data: bytes,
    })
}

/// `%N` → N (tmux pane id).
fn parse_pct_id(s: &str) -> Option<u32> {
    s.strip_prefix('%')?.parse().ok()
}

/// `@N` → N (tmux window id).
fn parse_at_id(s: &str) -> Option<u32> {
    s.strip_prefix('@')?.parse().ok()
}

/// `$N` → N (tmux session id).
fn parse_dollar_id(s: &str) -> Option<u32> {
    s.strip_prefix('$')?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(input: &[u8]) -> Vec<TmuxEvent> {
        let mut p = TmuxControlParser::new();
        p.feed(input);
        p.drain()
    }

    #[test]
    fn begin_end_roundtrip() {
        let evs = drain(b"%begin 1234 5 0\n%end 1234 5 0\n");
        assert_eq!(
            evs,
            vec![
                TmuxEvent::Begin {
                    time: 1234,
                    seq: 5,
                    flags: 0,
                },
                TmuxEvent::End {
                    time: 1234,
                    seq: 5,
                    flags: 0,
                },
            ]
        );
    }

    #[test]
    fn output_pane_id_and_octal_decode() {
        // tmux escapes \r\n inside output as \015\012.
        let evs = drain(b"%output %3 hello\\015\\012world\n");
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            TmuxEvent::Output { pane_id, data } => {
                assert_eq!(*pane_id, 3);
                assert_eq!(data, b"hello\r\nworld");
            }
            other => panic!("expected Output, got {other:?}"),
        }
    }

    /// Cycle 858 (audit): octal 400..777 (256..511) exceeds a byte. tmux only
    /// emits 000..377 for real bytes, so a larger value is malformed — the
    /// event is dropped rather than silently truncated to a wrong byte.
    #[test]
    fn output_rejects_octal_byte_overflow() {
        assert!(
            drain(b"%output %1 x\\777\n").is_empty(),
            "octal > 377 must be rejected, not truncated"
        );
        // \377 = 255 is the maximum valid byte and still decodes.
        let evs = drain(b"%output %1 \\377\n");
        assert_eq!(evs.len(), 1);
        match &evs[0] {
            TmuxEvent::Output { data, .. } => assert_eq!(data, &[255u8]),
            other => panic!("expected Output, got {other:?}"),
        }
    }

    #[test]
    fn window_add_close_renamed() {
        let evs = drain(b"%window-add @5\n%window-renamed @5 build\n%window-close @5\n");
        assert_eq!(
            evs,
            vec![
                TmuxEvent::WindowAdd { window_id: 5 },
                TmuxEvent::WindowRenamed {
                    window_id: 5,
                    name: "build".to_string(),
                },
                TmuxEvent::WindowClose { window_id: 5 },
            ]
        );
    }

    #[test]
    fn session_events() {
        let evs = drain(b"%session-changed $2 main\n%session-renamed renamed\n");
        assert_eq!(
            evs,
            vec![
                TmuxEvent::SessionChanged {
                    session_id: 2,
                    name: "main".to_string(),
                },
                TmuxEvent::SessionRenamed {
                    name: "renamed".to_string(),
                },
            ]
        );
    }

    #[test]
    fn layout_change_keeps_layout_raw() {
        let evs = drain(b"%layout-change @1 a1b2,80x24,0,0,1\n");
        assert_eq!(
            evs,
            vec![TmuxEvent::LayoutChange {
                window_id: 1,
                layout: "a1b2,80x24,0,0,1".to_string(),
            }]
        );
    }

    #[test]
    fn exit_with_and_without_reason() {
        let evs = drain(b"%exit\n%exit graceful\n");
        assert_eq!(
            evs,
            vec![
                TmuxEvent::Exit { reason: None },
                TmuxEvent::Exit {
                    reason: Some("graceful".to_string()),
                },
            ]
        );
    }

    #[test]
    fn unknown_verb_preserves_line() {
        let evs = drain(b"%totally-new-verb arg1 arg2\n");
        assert_eq!(
            evs,
            vec![TmuxEvent::Unknown {
                line: "%totally-new-verb arg1 arg2".to_string(),
            }]
        );
    }

    #[test]
    fn non_pct_line_surfaces_as_outside_block() {
        let evs = drain(b"hello not-a-control-line\n");
        assert_eq!(
            evs,
            vec![TmuxEvent::OutsideBlock {
                line: "hello not-a-control-line".to_string(),
            }]
        );
    }

    #[test]
    fn partial_line_held_until_next_feed() {
        let mut p = TmuxControlParser::new();
        p.feed(b"%begin 1 2");
        assert!(p.next_event().is_none());
        p.feed(b" 0\n");
        assert_eq!(
            p.next_event(),
            Some(TmuxEvent::Begin {
                time: 1,
                seq: 2,
                flags: 0,
            })
        );
    }

    #[test]
    fn crlf_line_endings_are_tolerated() {
        let evs = drain(b"%window-add @7\r\n");
        assert_eq!(evs, vec![TmuxEvent::WindowAdd { window_id: 7 }]);
    }

    #[test]
    fn oversize_line_dropped_without_stalling() {
        // 100 KB of garbage on one line — must not OOM, must not
        // stall the parser. Following normal line should still parse.
        let mut p = TmuxControlParser::new();
        let big = vec![b'x'; 100_000];
        p.feed(&big);
        p.feed(b"\n%window-add @1\n");
        // The garbage line gets dropped (no '%' prefix recovery from
        // overflow). The next line still arrives.
        let evs = p.drain();
        assert!(evs.contains(&TmuxEvent::WindowAdd { window_id: 1 }));
    }
}

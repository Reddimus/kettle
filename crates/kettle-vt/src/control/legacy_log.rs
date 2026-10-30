// Test-only snapshot of the session-log stripper before control-kernel consolidation.
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

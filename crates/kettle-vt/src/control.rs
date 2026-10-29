//! Streaming classification of terminal control bytes, independent of consumers.
//!
//! The kernel owns no buffers. Consumers decide whether to strip, accumulate,
//! or forward bytes from its events. UTF-8 continuations follow their lead's
//! classification even when recovery changes state in the middle of a scalar.

/// Maximum bytes following ESC before forced recovery. A C1 introducer counts
/// as the corresponding ESC follower. The overflowing byte is consumed;
/// a fresh ESC starts a new budget.
pub const MAX_CONTROL_SEQUENCE_BYTES: usize = 64 * 1024;

/// Wire form of a control sequence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Escape,
    Csi,
    Osc,
    Dcs,
    Sos,
    Pm,
    Apc,
    St,
}

/// Parser state after the last byte.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum State {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    Csi,
    String(Kind),
    /// ESC may finish the string with `\`, or dispatch a fresh escape.
    StringEscape(Kind),
}

/// A sequence's progress on this byte.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Action {
    Start,
    Continue,
    End,
    Cancel,
    Resync,
}

/// Classification of one input byte. Executed C0 controls belong to the
/// surrounding sequence but remain visible to stripping consumers.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Event {
    Text,
    Control {
        kind: Kind,
        action: Action,
        execute: bool,
    },
}

impl Event {
    /// Whether a transcript should retain this byte.
    pub fn is_text(self) -> bool {
        matches!(self, Self::Text | Self::Control { execute: true, .. })
    }
}

/// Allocation-free byte-at-a-time control parser. C1 introducers are recognized
/// only in Ground; unshielded ST also terminates strings. CAN/SUB cancel all
/// open sequences. ESC in a string abandons it and dispatches a fresh escape.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ControlParser {
    state: State,
    control_bytes: usize,
    utf8_continuation: u8,
    utf8_kind: Option<Kind>,
}

impl ControlParser {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn state(&self) -> State {
        self.state
    }

    /// Consume exactly one byte. A malformed UTF-8 lead stops shielding at the
    /// first non-continuation; no validation or replacement of text is done.
    pub fn advance(&mut self, byte: u8) -> Event {
        if self.utf8_continuation > 0 && (0x80..=0xbf).contains(&byte) {
            self.utf8_continuation -= 1;
            let overflow = self.count_byte();
            return match self.utf8_kind {
                None => Event::Text,
                Some(kind) => Self::control(
                    kind,
                    if overflow {
                        Action::Resync
                    } else {
                        Action::Continue
                    },
                    false,
                ),
            };
        }
        self.utf8_continuation = match byte {
            0xc2..=0xdf => 1,
            0xe0..=0xef => 2,
            0xf0..=0xf4 => 3,
            _ => 0,
        };
        let event = self.dispatch(byte);
        // Continuations share the event their lead returned, which can differ
        // from the state before it: a lead that ends an escape is an Escape.
        self.utf8_kind = match event {
            Event::Text => None,
            Event::Control { kind, .. } => Some(kind),
        };
        event
    }

    fn dispatch(&mut self, byte: u8) -> Event {
        if byte == 0x1b {
            self.control_bytes = 0;
            let (state, kind, action) = match self.state {
                State::String(kind) => (State::StringEscape(kind), kind, Action::Continue),
                _ => (State::Escape, Kind::Escape, Action::Start),
            };
            self.state = state;
            return Self::control(kind, action, false);
        }
        let kind = self.kind();
        if self.count_byte() {
            return Self::control(
                kind.expect("only open sequences count bytes"),
                Action::Resync,
                false,
            );
        }
        if byte == 0x18 || byte == 0x1a {
            return match kind {
                None => Event::Text,
                Some(kind) => {
                    self.state = State::Ground;
                    Self::control(kind, Action::Cancel, false)
                }
            };
        }

        match self.state {
            State::Ground => {
                let kind = match byte {
                    0x90 => Kind::Dcs,
                    0x98 => Kind::Sos,
                    0x9b => Kind::Csi,
                    0x9c => return Self::control(Kind::St, Action::End, false),
                    0x9d => Kind::Osc,
                    0x9e => Kind::Pm,
                    0x9f => Kind::Apc,
                    _ => return Event::Text,
                };
                self.control_bytes = 1;
                self.state = if kind == Kind::Csi {
                    State::Csi
                } else {
                    State::String(kind)
                };
                Self::control(kind, Action::Start, false)
            }
            State::Escape | State::StringEscape(_) => {
                let previous = self.state;
                self.state = match byte {
                    b'[' => State::Csi,
                    b']' => State::String(Kind::Osc),
                    b'P' => State::String(Kind::Dcs),
                    b'X' => State::String(Kind::Sos),
                    b'^' => State::String(Kind::Pm),
                    b'_' => State::String(Kind::Apc),
                    0x20..=0x2f => State::EscapeIntermediate,
                    0x00..=0x17 | 0x19 | 0x1c..=0x1f | 0x7f => State::Escape,
                    _ => State::Ground,
                };
                let kind = if byte == b'\\' {
                    match previous {
                        State::StringEscape(kind) => kind,
                        _ => Kind::Escape,
                    }
                } else {
                    self.kind().unwrap_or(Kind::Escape)
                };
                let action = match self.state {
                    State::Ground => Action::End,
                    State::Csi | State::String(_) => Action::Start,
                    _ => Action::Continue,
                };
                Self::control(kind, action, executes(byte))
            }
            State::EscapeIntermediate | State::Csi => {
                let kind = kind.expect("open sequence");
                let final_start = if self.state == State::Csi { 0x40 } else { 0x30 };
                let action = if (final_start..=0x7e).contains(&byte) {
                    self.state = State::Ground;
                    Action::End
                } else {
                    Action::Continue
                };
                Self::control(kind, action, executes(byte))
            }
            State::String(kind) => {
                let action = if byte == 0x9c || (kind == Kind::Osc && byte == 0x07) {
                    self.state = State::Ground;
                    Action::End
                } else {
                    Action::Continue
                };
                Self::control(kind, action, false)
            }
        }
    }

    fn kind(&self) -> Option<Kind> {
        match self.state {
            State::Ground => None,
            State::Escape | State::EscapeIntermediate => Some(Kind::Escape),
            State::Csi => Some(Kind::Csi),
            State::String(kind) | State::StringEscape(kind) => Some(kind),
        }
    }

    fn count_byte(&mut self) -> bool {
        if self.state == State::Ground {
            return false;
        }
        self.control_bytes += 1;
        if self.control_bytes > MAX_CONTROL_SEQUENCE_BYTES {
            self.state = State::Ground;
            true
        } else {
            false
        }
    }

    fn control(kind: Kind, action: Action, execute: bool) -> Event {
        Event::Control {
            kind,
            action,
            execute,
        }
    }
}

fn executes(byte: u8) -> bool {
    matches!(byte, 0x00..=0x17 | 0x19 | 0x1c..=0x1f)
}

/// Streaming transcript stripper shared by exec, MCP, and session logs.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct AnsiStripper {
    parser: ControlParser,
}

impl AnsiStripper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append text directly to the caller's buffer without an intermediate copy.
    pub fn push(&mut self, input: &[u8], out: &mut Vec<u8>) {
        let mut start = 0;
        for (index, &byte) in input.iter().enumerate() {
            if !self.parser.advance(byte).is_text() {
                out.extend_from_slice(&input[start..index]);
                start = index + 1;
            }
        }
        out.extend_from_slice(&input[start..]);
    }

    /// Strip one chunk, retaining parser state for the next call.
    pub fn strip(&mut self, input: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(input.len());
        self.push(input, &mut out);
        out
    }
}

#[cfg(test)]
#[path = "control/tests.rs"]
mod tests;

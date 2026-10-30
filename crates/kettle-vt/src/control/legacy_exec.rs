// Test-only snapshot of the exec stripper before control-kernel consolidation.
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
    /// recognizes (`0x90` DCS, `0x98` SOS, `0x9b` CSI, `0x9d` OSC, `0x9e` PM,
    /// `0x9f` APC) are all in the `0x80..=0xbf` continuation range. Treating
    /// them as controls wherever they appear would eat the middle of ordinary
    /// characters and leave invalid UTF-8 behind:
    ///
    /// * `Û` is `c3 9b`; reading `9b` as CSI would turn `Ûh` into a lone `c3`.
    /// * `‘` is `e2 80 98`; the `98` would read as SOS.
    /// * `▐` is `e2 96 90`; the `90` would read as DCS.
    ///
    /// Box-drawing and smart quotes are exactly what a TUI emits, and MCP's
    /// `kettle_run` strips by default, so this would corrupt the output an
    /// agent reads. Counting the sequence keeps a continuation byte a
    /// continuation byte; a C1 byte in ground position, where UTF-8 could not
    /// put one, is still honored for genuinely 8-bit streams.
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
            // decoded and can never be a control, in ANY state. Otherwise `0x9c`
            // inside an OSC reads as ST, so `ESC ] 0 ; ✳ title BEL` (`✳` is
            // `e2 9c b3`) ends the string early and leaks the rest as visible
            // text.
            if self.utf8_continuation > 0 {
                if matches!(b, 0x80..=0xbf) {
                    self.utf8_continuation -= 1;
                    // A continuation goes wherever its LEAD went, not wherever
                    // the state machine has since arrived. The forced
                    // resynchronization that ends an over-long control string
                    // can swallow a lead byte and return to ground. Emitting
                    // its continuations there would leave them with no lead,
                    // and stdout would be invalid UTF-8 from then on.
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
            // read. `ESC [` followed by 64 KiB of parameter bytes forces a
            // resynchronization out of `Csi`. If that bound lands on a lead
            // byte, the lead is consumed there, and untracked continuations
            // would then be emitted in ground with nothing in front of them.
            // `EscapeIntermediate` has the same bound, and `ESC` followed
            // directly by a lead byte splits a character the same way without
            // any 64 KiB.
            //
            // Tracking everywhere changes only malformed streams, because a
            // byte in `0xc2..=0xf4` is not a legal CSI parameter, intermediate,
            // or final byte. If the bytes that follow are NOT continuations,
            // the shield above releases them at once.
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
                StripState::Escape => {
                    if matches!(b, 0x00..=0x17 | 0x19 | 0x1c..=0x1f) {
                        out.push(b);
                    }
                    Self::after_escape(b)
                }
                StripState::EscapeIntermediate { remaining } => {
                    if matches!(b, 0x00..=0x17 | 0x19 | 0x1c..=0x1f) {
                        out.push(b);
                    }
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
                    if matches!(b, 0x00..=0x17 | 0x19 | 0x1c..=0x1f) {
                        out.push(b);
                    }
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
                    if escaped {
                        if matches!(b, 0x00..=0x17 | 0x19 | 0x1c..=0x1f) {
                            out.push(b);
                        }
                        Self::after_escape(b)
                    } else if matches!(b, 0x18 | 0x1a) || b == 0x9c || (bel_terminated && b == 0x07)
                    {
                        StripState::Ground
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
            0x00..=0x17 | 0x19 | 0x1b..=0x1f | 0x7f => StripState::Escape,
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

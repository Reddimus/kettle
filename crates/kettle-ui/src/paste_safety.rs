//! Linear, Unicode-safe inspection and removal of terminal controls.

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Analysis {
    pub lines: u64,
    pub escapes: u64,
    pub controls: u64,
}

pub(crate) fn analyse(text: &str) -> Analysis {
    scan(text, |_| {})
}

pub(crate) fn without_controls(text: &str) -> String {
    let mut clean = String::with_capacity(text.len());
    scan(text, |c| clean.push(c));
    clean
}

/// A kind of control found in pasted text, most serious first: the derived
/// order ranks the findings list and picks the control the preview must show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Kind {
    /// `ESC [ 201 ~`, which ends a bracketed paste.
    EndMarker,
    /// The same marker introduced by the C1 CSI, U+009B.
    EndMarkerC1,
    /// The bytes a terminal turns into a signal: 0x03, 0x1A and 0x1C.
    Interrupt,
    Suspend,
    Quit,
    /// 0x04, which a shell reads as the end of its input.
    EndOfInput,
    Escape,
    /// A bidirectional formatting character (an override, embedding, isolate
    /// or mark). It is not a control character to Unicode, but it changes the
    /// order text is drawn in, so a command can read differently from what
    /// it runs.
    Bidi(char),
    /// A C1 control, U+0080 to U+009F.
    C1(char),
    Backspace,
    Delete,
    Bell,
    /// Any other C0 control. TAB and the line breaks are never findings.
    Control(char),
}

impl Kind {
    /// The single byte this kind is on the wire, for the kinds that are one
    /// C0 or DEL byte. The encoder's own rule (`input::bracketed_paste_drops`)
    /// is asked about this byte, so the dialog never keeps a second list of
    /// what a bracketed paste removes.
    pub(crate) fn byte(self) -> Option<u8> {
        match self {
            Self::Interrupt => Some(0x03),
            Self::Suspend => Some(0x1a),
            Self::Quit => Some(0x1c),
            Self::EndOfInput => Some(0x04),
            Self::Escape => Some(0x1b),
            Self::Backspace => Some(0x08),
            Self::Delete => Some(0x7f),
            Self::Bell => Some(0x07),
            Self::Control(c) => u8::try_from(u32::from(c)).ok(),
            Self::EndMarker | Self::EndMarkerC1 | Self::Bidi(_) | Self::C1(_) => None,
        }
    }
}

/// One row of the findings: how often a kind occurs, and where it first does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Finding {
    pub kind: Kind,
    pub count: u64,
    /// Byte range of the first occurrence.
    pub first: (usize, usize),
}

const END_MARKER: &str = "\x1b[201~";
const END_MARKER_C1: &str = "\u{9b}201~";

/// Every kind of control in `text`, most serious first. One linear pass; at
/// most one entry per kind, so the result is bounded however long the text.
pub(crate) fn findings(text: &str) -> Vec<Finding> {
    let mut found = std::collections::BTreeMap::<Kind, Finding>::new();
    let mut index = 0;
    while let Some(c) = text[index..].chars().next() {
        let rest = &text[index..];
        let (kind, len) = if rest.starts_with(END_MARKER) {
            (Some(Kind::EndMarker), END_MARKER.len())
        } else if rest.starts_with(END_MARKER_C1) {
            (Some(Kind::EndMarkerC1), END_MARKER_C1.len())
        } else {
            let kind = match c {
                '\t' | '\n' | '\r' => None,
                '\x03' => Some(Kind::Interrupt),
                '\x1a' => Some(Kind::Suspend),
                '\x1c' => Some(Kind::Quit),
                '\x04' => Some(Kind::EndOfInput),
                '\x1b' => Some(Kind::Escape),
                '\x08' => Some(Kind::Backspace),
                '\x7f' => Some(Kind::Delete),
                '\x07' => Some(Kind::Bell),
                '\0'..='\x1f' => Some(Kind::Control(c)),
                '\u{80}'..='\u{9f}' => Some(Kind::C1(c)),
                c if kettle_config::ssh_config::is_bidi_format_char(c) => Some(Kind::Bidi(c)),
                _ => None,
            };
            (kind, c.len_utf8())
        };
        if let Some(kind) = kind {
            found
                .entry(kind)
                .or_insert(Finding {
                    kind,
                    count: 0,
                    first: (index, index + len),
                })
                .count += 1;
        }
        index += len;
    }
    found.into_values().collect()
}

fn scan(text: &str, mut emit: impl FnMut(char)) -> Analysis {
    let mut found = Analysis {
        lines: 1,
        ..Analysis::default()
    };
    let mut previous_cr = false;
    for c in text.chars() {
        if c == '\r' || (c == '\n' && !previous_cr) {
            found.lines += 1;
        }
        previous_cr = c == '\r';
    }
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        let introducer = match c {
            '\x1b' => {
                found.escapes += 1;
                match chars.peek().copied() {
                    Some('[' | ']' | 'P' | 'X' | '^' | '_') => chars.next().unwrap(),
                    _ => {
                        // ESC intermediates and their final byte form one sequence.
                        while chars.peek().is_some_and(|c| (' '..='/').contains(c)) {
                            chars.next();
                        }
                        if chars.peek().is_some_and(|c| ('0'..='~').contains(c)) {
                            chars.next();
                        }
                        continue;
                    }
                }
            }
            '\u{9b}' => {
                found.escapes += 1;
                '['
            }
            '\u{9d}' | '\u{90}' | '\u{98}' | '\u{9e}' | '\u{9f}' => {
                found.escapes += 1;
                if c == '\u{9d}' { ']' } else { 'P' }
            }
            // A bidirectional formatting character is counted and removed
            // with the controls: it is what lets pasted text show one command
            // and run another.
            c if (c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
                || kettle_config::ssh_config::is_bidi_format_char(c) =>
            {
                found.controls += 1;
                continue;
            }
            c => {
                emit(c);
                continue;
            }
        };
        if introducer == '[' {
            while let Some(&c) = chars.peek() {
                if c == '\x1b' || ('\u{80}'..='\u{9f}').contains(&c) {
                    break;
                }
                chars.next();
                if matches!(c, '\t' | '\n' | '\r') {
                    emit(c);
                } else if ('@'..='~').contains(&c) {
                    break;
                }
            }
        } else {
            while let Some(c) = chars.next() {
                if c == '\u{9c}' || (introducer == ']' && c == '\x07') {
                    break;
                }
                if c == '\x1b' && chars.peek() == Some(&'\\') {
                    chars.next();
                    break;
                }
                // Even malformed or unterminated strings cannot join commands.
                if matches!(c, '\t' | '\n' | '\r') {
                    emit(c);
                }
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_whole_sequences_and_preserves_utf8_tabs_and_line_endings() {
        let text = "café\x1b[31m界\x1b[0m\r\n\x1b]52;c;secret\x07two\n\tthree\r";
        assert_eq!(without_controls(text), "café界\r\ntwo\n\tthree\r");
        assert_eq!(
            analyse(text),
            Analysis {
                lines: 4,
                escapes: 3,
                controls: 0
            }
        );
        for sequence in [
            "\x1b(B",
            "\x1b7",
            "\x1b]title\x1b\\",
            "\x1bPdata\x1b\\",
            "\x1b_hidden\x1b\\",
            "\u{9b}31m",
            "\u{9d}title\u{9c}",
            "\u{90}data\u{9c}",
        ] {
            assert_eq!(
                without_controls(&format!("a{sequence}β")),
                "aβ",
                "{sequence:?}"
            );
            assert_eq!(analyse(sequence).escapes, 1);
        }
    }

    #[test]
    fn findings_rank_the_end_marker_first_and_count_each_kind_once() {
        let text = "a\tb\n\x07\x03x\x1b[31m\x03\u{9b}201~\x1b[201~\x1b[201~\x7f";
        let found = findings(text);
        let kinds: Vec<_> = found.iter().map(|f| (f.kind, f.count)).collect();
        assert_eq!(
            kinds,
            [
                (Kind::EndMarker, 2),
                (Kind::EndMarkerC1, 1),
                (Kind::Interrupt, 2),
                (Kind::Escape, 1),
                (Kind::Delete, 1),
                (Kind::Bell, 1),
            ]
        );
        // The first occurrence is a byte range of the text, the whole marker.
        let (start, end) = found[0].first;
        assert_eq!(&text[start..end], "\x1b[201~");
        let (start, end) = found[1].first;
        assert_eq!(&text[start..end], "\u{9b}201~");
        assert_eq!(found[2].first, (5, 6));
    }

    #[test]
    fn tab_and_line_breaks_are_never_findings() {
        assert!(findings("plain\ttext\r\nnext\nlast\r").is_empty());
        assert!(findings("").is_empty());
        assert!(findings("café 界 ñ").is_empty());
        for c in (0..=0x9fu32).filter_map(char::from_u32) {
            let found = findings(&c.to_string());
            let expected = c.is_control() && !matches!(c, '\t' | '\n' | '\r');
            assert_eq!(found.len(), usize::from(expected), "{c:?}");
        }
        let kinds = |text: &str| {
            findings(text)
                .into_iter()
                .map(|f| f.kind)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            kinds("\x1c\x1a\x04\x08\x01\u{85}\0"),
            [
                Kind::Suspend,
                Kind::Quit,
                Kind::EndOfInput,
                Kind::C1('\u{85}'),
                Kind::Backspace,
                Kind::Control('\0'),
                Kind::Control('\x01'),
            ]
        );
        // A marker cut short is an ESC, or a C1 control, like any other.
        assert_eq!(kinds("\x1b[201"), [Kind::Escape]);
        assert_eq!(kinds("\u{9b}20"), [Kind::C1('\u{9b}')]);
    }

    #[test]
    fn bidi_formatting_is_found_counted_and_removed_with_the_controls() {
        // Every character `is_bidi_format_char` names: overrides, embeddings,
        // isolates and the three marks.
        let all: Vec<char> = ('\u{202a}'..='\u{202e}')
            .chain('\u{2066}'..='\u{2069}')
            .chain(['\u{200e}', '\u{200f}', '\u{61c}'])
            .collect();
        for &c in &all {
            let text = format!("rm{c}-rf");
            assert_eq!(
                findings(&text)
                    .iter()
                    .map(|f| (f.kind, f.count))
                    .collect::<Vec<_>>(),
                [(Kind::Bidi(c), 1)],
                "{c:?}"
            );
            assert_eq!(analyse(&text).controls, 1, "{c:?}");
            assert_eq!(without_controls(&text), "rm-rf", "{c:?}");
            assert_eq!(Kind::Bidi(c).byte(), None);
        }
        // An override ranks below ESC and above the other 8-bit controls.
        let kinds: Vec<_> = findings("\u{85}\u{202e}\x1b")
            .into_iter()
            .map(|f| f.kind)
            .collect();
        assert_eq!(
            kinds,
            [Kind::Escape, Kind::Bidi('\u{202e}'), Kind::C1('\u{85}')]
        );
        // Ordinary right-to-left text has no formatting character in it.
        assert!(findings("שלום مرحبا").is_empty());
        assert_eq!(without_controls("שלום مرحبا"), "שלום مرحبا");
    }

    #[test]
    fn a_kind_names_its_byte_when_it_is_one() {
        for (kind, byte) in [
            (Kind::Interrupt, 0x03),
            (Kind::Suspend, 0x1a),
            (Kind::Quit, 0x1c),
            (Kind::EndOfInput, 0x04),
            (Kind::Escape, 0x1b),
            (Kind::Backspace, 0x08),
            (Kind::Delete, 0x7f),
            (Kind::Bell, 0x07),
            (Kind::Control('\x19'), 0x19),
        ] {
            assert_eq!(kind.byte(), Some(byte));
            // The byte is the one `findings` files under that kind.
            let text = char::from(byte).to_string();
            assert_eq!(findings(&text)[0].kind, kind);
        }
        for kind in [Kind::EndMarker, Kind::EndMarkerC1, Kind::C1('\u{9b}')] {
            assert_eq!(kind.byte(), None);
        }
    }

    #[test]
    fn strips_c0_del_and_c1_without_touching_utf8_continuation_bytes() {
        for c in (0..=0x9f)
            .filter_map(char::from_u32)
            .filter(|c| c.is_control())
        {
            let text = format!("é{c}界");
            let clean = without_controls(&text);
            if matches!(c, '\t' | '\r' | '\n') {
                assert_eq!(clean, text);
                assert_eq!(analyse(&text).controls, 0);
            } else {
                assert!(!clean.contains(c));
            }
        }
        assert_eq!(without_controls("é\x7f\u{85}\x03界"), "é界");
        assert_eq!(analyse("\x7f\u{85}\x03").controls, 3);
    }

    #[test]
    fn unterminated_and_nested_sequences_fail_closed_without_losing_newlines() {
        assert_eq!(without_controls("x\x1b[31\x1b[0my"), "xy");
        assert_eq!(analyse("\x1b[31\x1b[0m").escapes, 2);
        for sequence in ["\x1b[123", "\x1b]secret", "\u{9d}secret", "\x1bPsecret"] {
            assert_eq!(without_controls(&format!("ok{sequence}\r\n")), "ok\r\n");
        }
        assert_eq!(without_controls("a\x1bé界\x1b"), "aé界");
        assert_eq!(analyse("a\r\nb\rc\nd").lines, 4);
        assert_eq!(analyse("").lines, 1);
    }
}

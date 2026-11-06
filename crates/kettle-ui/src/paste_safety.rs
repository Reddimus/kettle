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
            c if c.is_control() && !matches!(c, '\t' | '\n' | '\r') => {
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

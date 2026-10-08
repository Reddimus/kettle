//! The eight-mark inline-card encoding, separate from Kitty image ids.

use crate::placeholder::{diacritic, diacritic_value};

const RADIX: u8 = 108;

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct InlineNonce([u8; 6]);

impl InlineNonce {
    pub fn new(digits: [u8; 6]) -> Option<Self> {
        digits
            .iter()
            .all(|&digit| digit < RADIX)
            .then_some(Self(digits))
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InlineMarker {
    pub row: u8,
    pub column: u8,
    pub nonce: InlineNonce,
}

impl InlineMarker {
    pub fn decode(marks: &[char]) -> Option<Self> {
        let marks: &[char; 8] = marks.try_into().ok()?;
        let mut digits = [0; 8];
        for (digit, &mark) in digits.iter_mut().zip(marks) {
            *digit = u8::try_from(diacritic_value(mark)?).ok()?;
            if *digit >= RADIX {
                return None;
            }
        }
        Some(Self {
            row: digits[0],
            column: digits[1],
            nonce: InlineNonce::new(digits[2..].try_into().ok()?)?,
        })
    }

    pub fn encode(self) -> Option<String> {
        if self.row >= RADIX || self.column >= RADIX {
            return None;
        }
        let mut text = String::with_capacity(20);
        text.push(crate::placeholder::PLACEHOLDER);
        for digit in [self.row, self.column].into_iter().chain(self.nonce.0) {
            text.push(diacritic(u16::from(digit))?);
        }
        Some(text)
    }
}

/// `text` with every placeholder sequence taken out: each `U+10EEEE` and
/// the placeholder diacritics after it, the form a card's marks or a Kitty
/// image's take. Text from outside that Kettle shows or names never carries
/// one; other combining marks stay.
pub fn strip_placeholders(text: &str) -> std::borrow::Cow<'_, str> {
    if !text.contains(crate::placeholder::PLACEHOLDER) {
        return std::borrow::Cow::Borrowed(text);
    }
    let mut kept = String::with_capacity(text.len());
    let mut in_sequence = false;
    for c in text.chars() {
        if c == crate::placeholder::PLACEHOLDER {
            in_sequence = true;
        } else if !(in_sequence && diacritic_value(c).is_some()) {
            in_sequence = false;
            kept.push(c);
        }
    }
    std::borrow::Cow::Owned(kept)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_placeholder_sequence_is_stripped_whole_and_other_marks_stay() {
        let marker = InlineMarker {
            row: 0,
            column: 1,
            nonce: InlineNonce::new([2, 3, 4, 5, 6, 7]).unwrap(),
        }
        .encode()
        .unwrap();
        assert_eq!(strip_placeholders(&format!("plot{marker} x")), "plot x");
        assert_eq!(strip_placeholders(&format!("{marker}{marker}")), "");
        // A mark that follows ordinary text is that text's, not a card's.
        assert_eq!(strip_placeholders("e\u{0305}"), "e\u{0305}");
        assert!(matches!(
            strip_placeholders("caf\u{e9}"),
            std::borrow::Cow::Borrowed(_)
        ));
    }

    #[test]
    fn independent_wire_fixture_has_all_eight_marks_and_twenty_bytes() {
        let marker = InlineMarker {
            row: 0,
            column: 1,
            nonce: InlineNonce::new([2, 3, 4, 5, 6, 7]).unwrap(),
        };
        let expected = "\u{10eeee}\u{0305}\u{030d}\u{030e}\u{0310}\u{0312}\u{033d}\u{033e}\u{033f}";
        assert_eq!(marker.encode().unwrap(), expected);
        assert_eq!(expected.len(), 20);
        assert_eq!(
            InlineMarker::decode(&expected.chars().skip(1).collect::<Vec<_>>()),
            Some(marker)
        );
    }

    #[test]
    fn complete_sequences_only_and_three_byte_or_foreign_marks_are_refused() {
        let valid = ['\u{0305}'; 8];
        assert!(InlineMarker::decode(&valid).is_some());
        assert!(InlineMarker::decode(&valid[..7]).is_none());
        assert!(InlineMarker::decode(&['\u{0305}'; 9]).is_none());
        for replacement in ['x', '\u{0951}'] {
            let mut invalid = valid;
            invalid[7] = replacement;
            assert!(InlineMarker::decode(&invalid).is_none());
        }
    }

    #[test]
    fn two_byte_alphabet_end_is_exact_and_invalid_coordinates_are_not_wrapped() {
        let nonce = InlineNonce::new([107; 6]).unwrap();
        let marker = InlineMarker {
            row: 107,
            column: 107,
            nonce,
        };
        let expected: String = std::iter::once('\u{10eeee}')
            .chain(std::iter::repeat_n('\u{07f3}', 8))
            .collect();
        assert_eq!(marker.encode().unwrap(), expected);
        assert_eq!(expected.len(), 20);
        assert!(InlineNonce::new([108; 6]).is_none());
        assert!(InlineMarker { row: 108, ..marker }.encode().is_none());
        assert!(
            InlineMarker {
                column: 108,
                ..marker
            }
            .encode()
            .is_none()
        );
    }

    #[test]
    fn each_nonce_digit_survives_independently_without_color_input() {
        for position in 0..6 {
            for digit in 0..108 {
                let mut digits = [0; 6];
                digits[position] = digit;
                let marker = InlineMarker {
                    row: 2,
                    column: 3,
                    nonce: InlineNonce::new(digits).unwrap(),
                };
                let encoded = marker.encode().unwrap();
                assert_eq!(encoded.len(), 20);
                assert_eq!(
                    InlineMarker::decode(&encoded.chars().skip(1).collect::<Vec<_>>()),
                    Some(marker)
                );
            }
        }
    }
}

//! Text extraction of inline-card cells without joining adjacent tokens.

use std::collections::TryReserveError;
use std::iter::Peekable;
use std::ops::Range;
use std::str::CharIndices;
use unicode_width::UnicodeWidthChar as _;

pub(crate) fn is_card_cell(base: char, marks: &[char]) -> bool {
    base == kettle_vt::placeholder::PLACEHOLDER && marks.len() > 4
}

pub(crate) fn text_parts(base: char, marks: &[char]) -> (char, &[char]) {
    if is_card_cell(base, marks) {
        (' ', &[])
    } else {
        (base, marks)
    }
}

struct MarkerRanges<'a>(Peekable<CharIndices<'a>>);

impl Iterator for MarkerRanges<'_> {
    type Item = Range<usize>;

    fn next(&mut self) -> Option<Self::Item> {
        while let Some((start, base)) = self.0.next() {
            if base != kettle_vt::placeholder::PLACEHOLDER {
                continue;
            }
            let mut end = start + base.len_utf8();
            let mut marks = 0;
            while let Some(&(at, mark)) = self.0.peek() {
                if mark.is_control() || mark.width() != Some(0) {
                    break;
                }
                self.0.next();
                marks += 1;
                end = at + mark.len_utf8();
            }
            if marks > 4 {
                return Some(start..end);
            }
        }
        None
    }
}

fn marker_ranges(text: &str) -> MarkerRanges<'_> {
    MarkerRanges(text.char_indices().peekable())
}

/// Replace each complete long placeholder cluster with one ASCII space.
/// Ordinary text and four-mark Kitty placeholders stay byte-identical. A space
/// preserves the token boundary between text on either side of a card cell.
/// The two linear scans reserve only the exact resulting size; unchanged input
/// reuses its original allocation. Allocation failure leaves callers free to
/// preserve the clipboard and report a control-response error.
pub fn scrub_card_markers(text: String) -> Result<String, TryReserveError> {
    let removed = marker_ranges(&text)
        .map(|range| range.len() - 1)
        .sum::<usize>();
    if removed == 0 {
        return Ok(text);
    }
    let mut clean = String::new();
    clean.try_reserve_exact(text.len() - removed)?;
    let mut previous = 0;
    for range in marker_ranges(&text) {
        clean.push_str(&text[previous..range.start]);
        clean.push(' ');
        previous = range.end;
    }
    clean.push_str(&text[previous..]);
    Ok(clean)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marker(count: usize) -> String {
        std::iter::once(kettle_vt::placeholder::PLACEHOLDER)
            .chain(std::iter::repeat_n('\u{0305}', count))
            .collect()
    }

    #[test]
    fn selection_replaces_each_card_cell_without_joining_tokens() {
        let input = format!(
            "out{}diagram.png\nhttps://exa{}mple.test",
            marker(8),
            marker(5)
        );
        assert_eq!(
            scrub_card_markers(input).unwrap(),
            "out diagram.png\nhttps://exa mple.test"
        );
    }

    #[test]
    fn ordinary_unicode_and_short_kitty_clusters_keep_exact_bytes_and_allocation() {
        let input = format!("e\u{0301} 👩\u{200d}💻 世界\u{fe0f} {}", marker(4));
        let expected = input.clone();
        let pointer = input.as_ptr();
        let clean = scrub_card_markers(input).unwrap();
        assert_eq!(clean, expected);
        assert_eq!(clean.as_ptr(), pointer);
    }

    #[test]
    fn complete_long_clusters_are_removed_without_consuming_adjacent_unicode() {
        let input = format!("é{}{}界", marker(32769), marker(8));
        assert_eq!(scrub_card_markers(input).unwrap(), "é  界");
    }

    #[test]
    fn control_characters_and_following_combining_graphemes_keep_their_boundaries() {
        let input = format!("{}\tA\u{0301}\n{}\u{0000}B", marker(8), marker(8));
        assert_eq!(
            scrub_card_markers(input).unwrap(),
            " \tA\u{0301}\n \u{0000}B"
        );
    }
}

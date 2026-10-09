//! Mermaid createText rows, using the renderer's whitespace and word boundaries.

use kettle_media::{FailureCode, MAX_MERMAID_BYTES};

pub(crate) const MAX_ROWS: usize = 256;
const MAX_ROW_XML_BYTES: usize = 1024 * 1024;

fn svg_space(c: char) -> bool {
    matches!(
        c,
        '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{a0}' | '\u{1680}' | '\u{2000}'
            ..='\u{200a}'
                | '\u{2028}'
                | '\u{2029}'
                | '\u{202f}'
                | '\u{205f}'
                | '\u{3000}'
                | '\u{feff}'
    )
}

pub(super) fn html_space(c: char) -> bool {
    matches!(c, '\t' | '\n' | '\u{c}' | '\r' | ' ')
}

fn break_length(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 4
        || bytes[0] != b'<'
        || !bytes[1].eq_ignore_ascii_case(&b'b')
        || !bytes[2].eq_ignore_ascii_case(&b'r')
    {
        return None;
    }
    let mut end = 3;
    while bytes
        .get(end)
        .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
    {
        end += 1;
    }
    if bytes.get(end) == Some(&b'/') {
        end += 1;
    }
    (bytes.get(end) == Some(&b'>')).then_some(end + 1)
}

fn normalize_breaks(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut offset = 0;
    while offset < text.len() {
        let suffix = &text[offset..];
        if let Some(length) = break_length(suffix.as_bytes()) {
            normalized.push('\n');
            offset += length;
        } else {
            let c = suffix
                .chars()
                .next()
                .expect("remaining UTF-8 starts on a scalar");
            normalized.push(c);
            offset += c.len_utf8();
        }
    }
    normalized
}

pub(super) fn html_break_lines(text: &str) -> Result<Vec<&str>, FailureCode> {
    if text.len() > MAX_MERMAID_BYTES {
        return Err(FailureCode::RenderResource);
    }
    let mut lines = Vec::new();
    let mut start = 0;
    let mut offset = 0;
    while offset < text.len() {
        if let Some(length) = break_length(&text.as_bytes()[offset..]) {
            if lines.len() == MAX_ROWS - 1 {
                return Err(FailureCode::RenderResource);
            }
            lines.push(&text[start..offset]);
            offset += length;
            start = offset;
        } else {
            offset += 1;
        }
    }
    lines.push(&text[start..]);
    Ok(lines)
}

struct Words<'a> {
    remaining: &'a str,
    no_closing_angle: bool,
}

impl<'a> Iterator for Words<'a> {
    type Item = &'a str;

    fn next(&mut self) -> Option<Self::Item> {
        while !self.remaining.is_empty() {
            let c = self.remaining.chars().next()?;
            if c == '<' {
                if !self.no_closing_angle {
                    match self.remaining[1..].find('>') {
                        Some(end) if end > 0 => {
                            let (word, tail) = self.remaining.split_at(end + 2);
                            self.remaining = tail;
                            return Some(word);
                        }
                        None => self.no_closing_angle = true,
                        _ => {}
                    }
                }
                self.remaining = &self.remaining[1..];
            } else if c == '>' || svg_space(c) {
                self.remaining = &self.remaining[c.len_utf8()..];
            } else {
                let end = self
                    .remaining
                    .char_indices()
                    .find(|(_, c)| matches!(c, '<' | '>') || svg_space(*c))
                    .map_or(self.remaining.len(), |(i, _)| i);
                let (word, tail) = self.remaining.split_at(end);
                self.remaining = tail;
                return Some(word);
            }
        }
        None
    }
}

pub(crate) fn formatted_rows(text: &str) -> Result<Vec<String>, FailureCode> {
    if text.len() > MAX_MERMAID_BYTES {
        return Err(FailureCode::RenderResource);
    }
    let normalized = normalize_breaks(text);
    let mut rows: Vec<_> = normalized.split('\n').collect();
    while rows.len() > 1
        && rows
            .last()
            .is_some_and(|row| row.trim_matches(html_space).is_empty())
    {
        rows.pop();
    }
    if rows.len() > MAX_ROWS {
        return Err(FailureCode::RenderResource);
    }
    let mut output = Vec::with_capacity(rows.len());
    let mut bytes = 0;
    for row in rows {
        let mut xml = String::new();
        for (index, word) in (Words {
            remaining: row,
            no_closing_angle: false,
        })
        .enumerate()
        {
            xml.push_str("<tspan font-style='normal' font-weight='normal'>");
            if index > 0 {
                xml.push(' ');
            }
            super::text_metrics::escape_xml(word, &mut xml);
            xml.push_str("</tspan>");
            if bytes + xml.len() > MAX_ROW_XML_BYTES {
                return Err(FailureCode::RenderResource);
            }
        }
        bytes += xml.len();
        output.push(xml);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(text: &str) -> Vec<&str> {
        Words {
            remaining: text,
            no_closing_angle: false,
        }
        .collect()
    }

    #[test]
    fn whitespace_boundaries_match_svg_labels_and_preserve_nel() {
        assert_eq!(
            words(" A\tB\u{a0}C\u{2000}D\u{85}E\u{feff}F "),
            ["A", "B", "C", "D\u{85}E", "F"]
        );
    }

    #[test]
    fn tags_are_literal_words_and_malformed_angles_do_not_rescan_suffixes() {
        assert_eq!(
            words("a<strong>x</strong> <> <open tail"),
            ["a", "<strong>", "x", "</strong>", "open", "tail"]
        );
        assert_eq!(words(&format!("{}tail", "< ".repeat(8192))), ["tail"]);
    }

    #[test]
    fn formatted_words_are_separate_normal_spans_with_literal_text() {
        let rows = formatted_rows("A   B <em>C</em> &\"").unwrap();
        assert_eq!(rows[0].matches("font-weight='normal'").count(), 6);
        assert!(rows[0].contains("> B</tspan>"));
        assert!(rows[0].contains("> &lt;em&gt;</tspan>"));
        assert!(rows[0].contains("> &amp;&quot;</tspan>"));
    }

    #[test]
    fn break_variants_preserve_interior_empty_rows_and_trim_ascii_tail() {
        let rows = formatted_rows("A<BR />\nB<br> \t\n").unwrap();
        assert_eq!(rows.len(), 3);
        assert!(rows[1].is_empty());
        assert_eq!(formatted_rows("A\n\u{a0}").unwrap().len(), 2);
        assert_eq!(formatted_rows("\n\n").unwrap(), [String::new()]);
        assert_eq!(formatted_rows("A<br / >B").unwrap().len(), 1);
    }

    #[test]
    fn row_and_xml_limits_fail_before_native_shaping() {
        assert_eq!(
            formatted_rows(&"x\n".repeat(MAX_ROWS + 1)),
            Err(FailureCode::RenderResource)
        );
        assert_eq!(
            formatted_rows(&"x ".repeat(MAX_MERMAID_BYTES / 2)),
            Err(FailureCode::RenderResource)
        );
        assert_eq!(
            formatted_rows(&"x".repeat(MAX_MERMAID_BYTES + 1)),
            Err(FailureCode::RenderResource)
        );
    }
}

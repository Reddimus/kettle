//! Fallible greedy SVG wrapping using native widths and the original checkpoint.

use kettle_media::FailureCode;
use std::collections::VecDeque;
use unicode_segmentation::UnicodeSegmentation;

use super::text_rows::{MAX_ROWS, html_break_lines, html_space};

const MAX_PROBES: usize = 512;
const MAX_PROBE_BYTES: usize = 1024 * 1024;

fn html_segments(text: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut start = 0;
    for (end, opportunity) in unicode_linebreak::linebreaks(text) {
        // Merman's CSS line breaker keeps slash-separated URI/path components
        // together and adds a break after every preserved ASCII space.
        if opportunity == unicode_linebreak::BreakOpportunity::Allowed && text[..end].ends_with('/')
        {
            continue;
        }
        let mut part = start;
        for (offset, c) in text[start..end].char_indices() {
            if c == ' ' {
                let next = start + offset + 1;
                segments.push(&text[part..next]);
                part = next;
            }
        }
        if part < end {
            segments.push(&text[part..end]);
        }
        start = end;
    }
    if start < text.len() {
        segments.push(&text[start..]);
    }
    segments
}

/// Wrap one already-normalized HTML row, retaining unbreakable tokens and CSS
/// break-spaces. All native probes are fallible and retain their caller's clock.
pub(crate) fn html_line(
    text: &str,
    max_width: Option<f64>,
    break_spaces: bool,
    mut measure: impl FnMut(&str) -> Result<f64, FailureCode>,
    mut checkpoint: impl FnMut() -> Result<(), FailureCode>,
) -> Result<Vec<String>, FailureCode> {
    checkpoint()?;
    if text.len() > kettle_media::MAX_MERMAID_BYTES {
        return Err(FailureCode::RenderResource);
    }
    let Some(limit) = max_width.filter(|width| width.is_finite() && *width > 0.0) else {
        return Ok(vec![text.to_owned()]);
    };
    let mut tokens = if break_spaces {
        VecDeque::from(html_segments(text))
    } else {
        let mut tokens = VecDeque::new();
        for part in text.split(' ') {
            if !part.is_empty() {
                tokens.push_back(part);
            }
            tokens.push_back(" ");
        }
        while tokens.back() == Some(&" ") {
            tokens.pop_back();
        }
        tokens
    };
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut budget = Budget::default();
    while let Some(token) = tokens.pop_front() {
        checkpoint()?;
        if !break_spaces && current.is_empty() && token == " " {
            continue;
        }
        let previous_length = current.len();
        current.push_str(token);
        if budget.width(&current, &mut measure, &mut checkpoint)? <= limit {
            continue;
        }
        current.truncate(previous_length);
        let has_content = if break_spaces {
            !current.is_empty()
        } else {
            !current.trim_matches(html_space).is_empty()
        };
        if has_content {
            let row = if break_spaces {
                current.as_str()
            } else {
                current.trim_end_matches(html_space)
            };
            push_row(&mut rows, row)?;
            current.clear();
            tokens.push_front(token);
        } else if break_spaces || token != " " {
            push_row(&mut rows, token)?;
        }
    }
    if break_spaces && !current.is_empty() {
        push_row(&mut rows, &current)?;
    } else if !current.trim_matches(html_space).is_empty() {
        push_row(&mut rows, current.trim_end_matches(html_space))?;
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    checkpoint()?;
    Ok(rows)
}

#[derive(Default)]
struct Budget {
    probes: usize,
    bytes: usize,
}

impl Budget {
    fn width(
        &mut self,
        text: &str,
        measure: &mut impl FnMut(&str) -> Result<f64, FailureCode>,
        checkpoint: &mut impl FnMut() -> Result<(), FailureCode>,
    ) -> Result<f64, FailureCode> {
        checkpoint()?;
        self.probes = self
            .probes
            .checked_add(1)
            .ok_or(FailureCode::RenderResource)?;
        self.bytes = self
            .bytes
            .checked_add(text.len())
            .ok_or(FailureCode::RenderResource)?;
        if self.probes > MAX_PROBES || self.bytes > MAX_PROBE_BYTES {
            return Err(FailureCode::RenderResource);
        }
        let width = measure(text)?;
        checkpoint()?;
        if !width.is_finite() || width < 0.0 {
            return Err(FailureCode::RenderParse);
        }
        Ok(width)
    }
}

fn push_row(rows: &mut Vec<String>, text: &str) -> Result<(), FailureCode> {
    if rows.len() == MAX_ROWS {
        return Err(FailureCode::RenderResource);
    }
    rows.push(text.to_owned());
    Ok(())
}

pub(crate) fn lines(
    text: &str,
    max_width: Option<f64>,
    mut measure: impl FnMut(&str) -> Result<f64, FailureCode>,
    mut checkpoint: impl FnMut() -> Result<(), FailureCode>,
) -> Result<Vec<String>, FailureCode> {
    checkpoint()?;
    let source_lines = html_break_lines(text)?;
    let mut budget = Budget::default();
    let mut rows = Vec::new();
    for line in source_lines {
        checkpoint()?;
        let Some(limit) = max_width.filter(|width| width.is_finite() && *width > 0.0) else {
            push_row(&mut rows, line)?;
            continue;
        };
        let mut tokens = VecDeque::new();
        for part in line.split(' ') {
            if !part.is_empty() {
                tokens.push_back(part);
            }
            tokens.push_back(" ");
        }
        while tokens.back() == Some(&" ") {
            tokens.pop_back();
        }
        let previous_rows = rows.len();
        let mut current = String::new();
        while let Some(token) = tokens.pop_front() {
            checkpoint()?;
            if current.is_empty() && token == " " {
                continue;
            }
            let previous_length = current.len();
            current.push_str(token);
            let candidate = current.trim_end_matches(html_space);
            if budget.width(candidate, &mut measure, &mut checkpoint)? <= limit {
                continue;
            }
            current.truncate(previous_length);
            if !current.trim_matches(html_space).is_empty() {
                push_row(&mut rows, current.trim_end_matches(html_space))?;
                current.clear();
                tokens.push_front(token);
                continue;
            }
            if token == " " {
                continue;
            }
            // Advances need not be monotonic across combining marks or kerning.
            // Visit borrowed grapheme prefixes in order rather than binary-searching.
            // A wrapped row must not split a combining sequence or emoji cluster.
            let mut cut = 0;
            let mut previous_width = 0.0;
            let mut has_positive_advance = false;
            for (offset, grapheme) in token.grapheme_indices(true) {
                let next = offset + grapheme.len();
                let width = budget.width(&token[..next], &mut measure, &mut checkpoint)?;
                if has_positive_advance && width > previous_width && width > limit {
                    break;
                }
                cut = next;
                previous_width = width;
                has_positive_advance |= width > 0.0;
            }
            // An oversized first visible cluster stays with leading zero-width
            // clusters; those must not become an orphan wrapped row.
            if cut == 0 {
                cut = token
                    .graphemes(true)
                    .next()
                    .expect("tokens are nonempty")
                    .len();
            }
            push_row(&mut rows, &token[..cut])?;
            if cut < token.len() {
                tokens.push_front(&token[cut..]);
            }
        }
        if !current.trim_matches(html_space).is_empty() {
            push_row(&mut rows, current.trim_end_matches(html_space))?;
        }
        if rows.len() == previous_rows {
            push_row(&mut rows, "")?;
        }
    }
    checkpoint()?;
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn width(text: &str) -> Result<f64, FailureCode> {
        Ok(text.chars().count() as f64)
    }

    #[test]
    fn html_segments_keep_uri_components_nbsp_and_unicode_breaks() {
        assert_eq!(
            html_segments("https://x.test/(alpha)/z"),
            ["https://x.test/(alpha)/z"]
        );
        assert_eq!(html_segments("alpha-beta"), ["alpha-", "beta"]);
        assert_eq!(html_segments("a\u{a0}b"), ["a\u{a0}b"]);
        assert_eq!(
            html_segments("\u{8d1f}\u{8d23}\u{4eba}"),
            ["\u{8d1f}", "\u{8d23}", "\u{4eba}"]
        );
        assert_eq!(html_segments("a  b"), ["a ", " ", "b"]);
    }

    #[test]
    fn html_wrapping_preserves_break_spaces_and_does_not_split_min_content() {
        assert_eq!(
            html_line("a  b", Some(2.0), true, width, || Ok(())).unwrap(),
            ["a ", " b"]
        );
        assert_eq!(
            html_line("unbreakable", Some(2.0), true, width, || Ok(())).unwrap(),
            ["unbreakable"]
        );
        assert_eq!(
            html_line("a\u{a0}b", Some(1.0), true, width, || Ok(())).unwrap(),
            ["a\u{a0}b"]
        );
        assert_eq!(
            html_line(" a  b ", Some(8.0), false, width, || Ok(())).unwrap(),
            ["a  b"]
        );
    }

    #[test]
    fn html_native_failure_unwinds_before_another_token_or_probe() {
        let mut probes = 0;
        let result = html_line(
            "a b c d e",
            Some(2.0),
            true,
            |text| {
                probes += 1;
                if probes == 2 {
                    Err(FailureCode::RenderTimeout)
                } else {
                    width(text)
                }
            },
            || Ok(()),
        );
        assert_eq!(result, Err(FailureCode::RenderTimeout));
        assert_eq!(probes, 2);
        assert_eq!(
            html_line("", None, false, width, || Err(FailureCode::RenderTimeout)),
            Err(FailureCode::RenderTimeout)
        );
    }

    #[test]
    fn greedy_words_preserve_interior_spaces_and_remove_wrapping_edge_spaces() {
        assert_eq!(
            lines("  a  b   cd e  ", Some(6.0), width, || Ok(())).unwrap(),
            ["a  b", "cd e"]
        );
        assert_eq!(
            lines("a  b", Some(8.0), width, || Ok(())).unwrap(),
            ["a  b"]
        );
    }

    #[test]
    fn long_tokens_split_at_utf8_scalar_boundaries_and_always_make_progress() {
        assert_eq!(
            lines("αβγδε", Some(3.0), width, || Ok(())).unwrap(),
            ["αβγ", "δε"]
        );
        assert_eq!(
            lines("αβ", Some(0.1), width, || Ok(())).unwrap(),
            ["α", "β"]
        );
    }

    #[test]
    fn combining_zwj_and_flag_clusters_stay_whole_even_beyond_the_limit() {
        let accent = "a\u{301}";
        let family = "\u{1f469}\u{200d}\u{1f469}\u{200d}\u{1f467}";
        let flag = "\u{1f1ea}\u{1f1f8}";
        let input = format!("{accent}{family}{flag}");
        for limit in [0.1, 1.0] {
            let rows = lines(
                &input,
                Some(limit),
                |text| Ok(text.graphemes(true).count() as f64),
                || Ok(()),
            )
            .unwrap();
            assert_eq!(rows, [accent, family, flag]);
            assert_eq!(rows.concat(), input);
        }
    }

    #[test]
    fn leading_zero_advance_clusters_stay_with_the_first_visible_cluster() {
        let rows = lines(
            "\u{301}WW",
            Some(0.5),
            |text| Ok(text.chars().filter(|c| *c != '\u{301}').count() as f64),
            || Ok(()),
        )
        .unwrap();
        assert_eq!(rows, ["\u{301}W", "W"]);
    }

    #[test]
    fn borrowed_grapheme_prefixes_remain_under_the_original_probe_budget() {
        let cluster = "a\u{301}";
        let input = cluster.repeat(2048);
        let mut probes = 0;
        let mut bytes = 0;
        let result = lines(
            &input,
            Some(1.0),
            |text| {
                assert!(text.starts_with(cluster));
                assert!(text.ends_with(cluster));
                probes += 1;
                bytes += text.len();
                Ok(text.graphemes(true).count() as f64)
            },
            || Ok(()),
        );
        assert_eq!(result, Err(FailureCode::RenderResource));
        assert!(probes <= MAX_PROBES);
        assert!(bytes <= MAX_PROBE_BYTES);
    }

    #[test]
    fn explicit_html_breaks_keep_blank_rows_and_do_not_treat_newline_as_br() {
        assert_eq!(
            lines("x<br> <BR/>y<br>", Some(20.0), width, || Ok(())).unwrap(),
            ["x", "", "y", ""]
        );
        assert_eq!(lines("x\ny", None, width, || Ok(())).unwrap(), ["x\ny"]);
        assert_eq!(lines("", Some(20.0), width, || Ok(())).unwrap(), [""]);
    }

    #[test]
    fn unbounded_and_invalid_widths_preserve_source_without_native_probes() {
        for limit in [
            None,
            Some(0.0),
            Some(-1.0),
            Some(f64::NAN),
            Some(f64::INFINITY),
        ] {
            assert_eq!(
                lines(
                    " a b ",
                    limit,
                    |_| panic!("unwrapped text needs no width probe"),
                    || Ok(())
                )
                .unwrap(),
                [" a b "]
            );
        }
    }

    #[test]
    fn cancellation_on_empty_input_is_observed_before_work() {
        assert_eq!(
            lines(
                "",
                None,
                |_| panic!("cancelled operation cannot shape"),
                || Err(FailureCode::RenderTimeout)
            ),
            Err(FailureCode::RenderTimeout)
        );
    }

    #[test]
    fn native_failure_is_replayed_immediately_without_further_probes() {
        let mut calls = 0;
        let result = lines(
            "abcdef",
            Some(2.0),
            |text| {
                calls += 1;
                if calls == 2 {
                    Err(FailureCode::RenderTimeout)
                } else {
                    width(text)
                }
            },
            || Ok(()),
        );
        assert_eq!(result, Err(FailureCode::RenderTimeout));
        assert_eq!(calls, 2);
        assert_eq!(
            lines("x", Some(2.0), |_| Ok(f64::NAN), || Ok(())),
            Err(FailureCode::RenderParse)
        );
    }

    #[test]
    fn cumulative_prefix_work_fails_before_quadratic_payload_copies() {
        let mut probes = 0;
        let mut bytes = 0;
        let input = "x".repeat(kettle_media::MAX_MERMAID_BYTES);
        let result = lines(
            &input,
            Some(1.0),
            |text| {
                probes += 1;
                bytes += text.len();
                width(text)
            },
            || Ok(()),
        );
        assert_eq!(result, Err(FailureCode::RenderResource));
        assert!(probes <= MAX_PROBES);
        assert!(bytes <= MAX_PROBE_BYTES);
    }
}

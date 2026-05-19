//! Regex search across the whole buffer (scrollback + viewport), powering the
//! Ctrl+Shift+F overlay.

use alacritty_terminal::Term;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use regex::Regex;

use crate::event::EventProxy;

/// A match expressed in absolute grid coordinates (line can be negative for
/// scrollback).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Match {
    pub line: i32,
    pub start_col: usize,
    pub end_col: usize,
}

/// Compile a search pattern with **smart-case** semantics: case-insensitive
/// unless the pattern contains an uppercase letter (ripgrep/vim behavior).
/// The pattern is a real regex; if it doesn't compile it falls back to a
/// literal (escaped) search so a stray `(` or `*` never breaks search.
pub fn build_regex(pattern: &str) -> Option<Regex> {
    if pattern.is_empty() {
        return None;
    }
    let ci = !pattern.chars().any(|c| c.is_uppercase());
    let flag = if ci { "(?i)" } else { "" };
    Regex::new(&format!("{flag}{pattern}"))
        .or_else(|_| Regex::new(&format!("{flag}{}", regex::escape(pattern))))
        .ok()
}

pub fn search(term: &Term<EventProxy>, pattern: &str) -> Vec<Match> {
    let Some(re) = build_regex(pattern) else {
        return Vec::new();
    };

    let grid = term.grid();
    let cols = grid.columns();
    let top = grid.topmost_line().0;
    let bottom = grid.bottommost_line().0;

    let mut matches = Vec::new();
    for line in top..=bottom {
        // Reconstruct the line text, tracking the byte->column mapping.
        let mut text = String::with_capacity(cols);
        let mut col_of_byte: Vec<usize> = Vec::with_capacity(cols * 2);
        for c in 0..cols {
            let cell = &grid[Point::new(Line(line), Column(c))];
            let ch = cell.c;
            for b in 0..ch.len_utf8() {
                let _ = b;
                col_of_byte.push(c);
            }
            text.push(ch);
        }
        for m in re.find_iter(&text) {
            let start_col = col_of_byte.get(m.start()).copied().unwrap_or(0);
            let end_col = col_of_byte
                .get(m.end().saturating_sub(1))
                .copied()
                .unwrap_or(start_col);
            matches.push(Match {
                line,
                start_col,
                end_col,
            });
        }
    }
    matches
}

#[cfg(test)]
mod tests {
    use super::build_regex;

    #[test]
    fn smart_case_is_insensitive_until_an_uppercase() {
        // All-lowercase pattern → case-insensitive.
        let re = build_regex("error").unwrap();
        assert!(re.is_match("ERROR"));
        assert!(re.is_match("Error"));
        assert!(re.is_match("error"));
        // Any uppercase → case-sensitive.
        let re = build_regex("Error").unwrap();
        assert!(re.is_match("Error"));
        assert!(!re.is_match("error"));
        assert!(!re.is_match("ERROR"));
    }

    #[test]
    fn pattern_is_a_real_regex() {
        let re = build_regex(r"warn|fail").unwrap();
        assert!(re.is_match("a fail here"));
        assert!(re.is_match("WARN: x"), "alternation + smart-case");
        let re = build_regex(r"\bfoo\b").unwrap();
        assert!(re.is_match("a foo b"));
        assert!(!re.is_match("foobar"));
    }

    #[test]
    fn invalid_regex_falls_back_to_literal() {
        // Unbalanced paren is not a valid regex → literal search instead.
        let re = build_regex("a(b").unwrap();
        assert!(re.is_match("xx a(b yy"));
        assert!(!re.is_match("ab"));
        // Empty pattern yields nothing.
        assert!(build_regex("").is_none());
    }
}

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

pub fn search(term: &Term<EventProxy>, pattern: &str) -> Vec<Match> {
    let re = match Regex::new(&format!("(?i){}", regex::escape(pattern))) {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    if pattern.is_empty() {
        return Vec::new();
    }

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

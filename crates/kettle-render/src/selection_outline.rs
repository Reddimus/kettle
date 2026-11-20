use std::ops::Range;

use crate::{QuadInstance, Rgb, rect};

pub(crate) struct SelectionRun {
    pub row: i32,
    pub columns: Range<usize>,
    /// How many pixels the run's fill reaches past its left and right cell
    /// edges, over right-to-left text drawn wider than its cells.
    pub reach: [f32; 2],
}

/// Draw only the exposed edges of the union, inside the selection. Shared
/// row boundaries have no stroke, including the overlapping part of a step,
/// and runs whose reach meets on a row join without a seam. `runs` are
/// sorted by row, then column.
pub(crate) fn append_outline(
    target: &mut Vec<QuadInstance>,
    runs: &[SelectionRun],
    origin: (f32, f32),
    cell: (f32, f32),
    scale: f32,
    ink: Rgb,
) {
    let thickness = scale.round().max(1.0);
    // Each row's pixel spans, `(row, left, right)`.
    let mut spans: Vec<(i32, f32, f32)> = Vec::with_capacity(runs.len());
    for run in runs {
        let left = (origin.0 + run.columns.start as f32 * cell.0 - run.reach[0]).round();
        let right = (origin.0 + run.columns.end as f32 * cell.0 + run.reach[1]).round();
        match spans.last_mut() {
            Some((row, _, end)) if *row == run.row && left <= *end => *end = end.max(right),
            _ => spans.push((run.row, left, right)),
        }
    }
    let y = |row: i32| (origin.1 + row as f32 * cell.1).round();
    for &(row, left, right) in &spans {
        let (top, bottom) = (y(row), y(row + 1));
        let vertical = thickness.min((right - left).max(0.0));
        target.push(rect(left, top, vertical, bottom - top, ink, 1.0));
        target.push(rect(
            right - vertical,
            top,
            vertical,
            bottom - top,
            ink,
            1.0,
        ));
        for (neighbor, edge) in [(row - 1, top), (row + 1, bottom - thickness)] {
            let mut start = left;
            let first = spans.partition_point(|other| other.0 < neighbor);
            for &(_, other_left, other_right) in spans[first..]
                .iter()
                .take_while(|other| other.0 == neighbor)
            {
                let end = other_left.min(right);
                if end > start {
                    target.push(rect(start, edge, end - start, thickness, ink, 1.0));
                }
                start = start.max(other_right);
                if start >= right {
                    break;
                }
            }
            if start < right {
                target.push(rect(start, edge, right - start, thickness, ink, 1.0));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn outline_follows_exposed_union_edges_at_native_scales() {
        traces_the_union(&[
            (0, 1..5, [0, 0]),
            (1, 0..5, [0, 0]),
            (2, 0..3, [0, 0]),
            (3, 0..1, [0, 0]),
            (3, 2..3, [0, 0]),
        ]);
    }

    #[test]
    fn outline_follows_fills_reaching_past_their_cells() {
        traces_the_union(&[
            (0, 1..3, [4, 0]),
            (1, 0..5, [0, 6]),
            (2, 0..2, [3, 0]),
            (2, 3..5, [2, 2]),
            // Two runs whose reach meets across the cell between them.
            (3, 0..2, [0, 4]),
            (3, 3..5, [6, 0]),
        ]);
    }

    /// The outline of `runs` (row, columns, reach in 1x pixels) paints
    /// exactly the union's edge pixels, at 1x and 2x.
    fn traces_the_union(runs: &[(i32, Range<usize>, [i32; 2])]) {
        for scale in [1, 2] {
            let (cw, ch, ox, oy) = (10 * scale, 12 * scale, 3 * scale, 5 * scale);
            let runs: Vec<_> = runs
                .iter()
                .map(|(row, columns, reach)| SelectionRun {
                    row: *row,
                    columns: columns.clone(),
                    reach: reach.map(|px| (px * scale) as f32),
                })
                .collect();
            let mut selected = HashSet::new();
            for run in &runs {
                let left = ox + run.columns.start as i32 * cw - run.reach[0] as i32;
                let right = ox + run.columns.end as i32 * cw + run.reach[1] as i32;
                for y in oy + run.row * ch..oy + (run.row + 1) * ch {
                    for x in left..right {
                        selected.insert((x, y));
                    }
                }
            }
            let mut quads = Vec::new();
            append_outline(
                &mut quads,
                &runs,
                (ox as f32, oy as f32),
                (cw as f32, ch as f32),
                scale as f32,
                Rgb::new(255, 255, 255),
            );
            assert!(!quads.is_empty());
            let mut painted = HashSet::new();
            for quad in quads {
                let [x, y] = quad.pos;
                let [w, h] = quad.size;
                assert_eq!([x.fract(), y.fract(), w.fract(), h.fract()], [0.0; 4]);
                for py in y as i32..(y + h) as i32 {
                    for px in x as i32..(x + w) as i32 {
                        painted.insert((px, py));
                    }
                }
            }
            let expected: HashSet<_> = selected
                .iter()
                .copied()
                .filter(|&(x, y)| {
                    (1..=scale).any(|d| {
                        [(x - d, y), (x + d, y), (x, y - d), (x, y + d)]
                            .iter()
                            .any(|p| !selected.contains(p))
                    })
                })
                .collect();
            assert_eq!(
                painted, expected,
                "scale {scale}: union perimeter, without internal seams"
            );
        }
    }
}

use std::ops::Range;

use crate::{QuadInstance, Rgb, rect};

pub(crate) struct SelectionRun {
    pub row: i32,
    pub columns: Range<usize>,
    /// The x the run's fill's left and right sides moved to, past its cell
    /// edges, over right-to-left text drawn wider than its cells.
    pub moved: [Option<f32>; 2],
}

/// Draw only the exposed edges of the union, inside the selection. Shared
/// row boundaries have no stroke, including the overlapping part of a step,
/// and runs whose moved sides meet on a row join without a seam. `runs` are
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
    let x = |column: usize| origin.0 + column as f32 * cell.0;
    // Each row's pixel spans, `(row, left, right, right side moved)`.
    let mut spans: Vec<(i32, f32, f32, bool)> = Vec::with_capacity(runs.len());
    for run in runs {
        let left = run.moved[0].unwrap_or_else(|| x(run.columns.start)).round();
        let right = run.moved[1].unwrap_or_else(|| x(run.columns.end)).round();
        match spans.last_mut() {
            Some((row, _, end, moved))
                if *row == run.row && (*moved || run.moved[0].is_some()) && left <= *end =>
            {
                *end = end.max(right);
                *moved = run.moved[1].is_some();
            }
            _ => spans.push((run.row, left, right, run.moved[1].is_some())),
        }
    }
    let y = |row: i32| (origin.1 + row as f32 * cell.1).round();
    for &(row, left, right, _) in &spans {
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
            for &(_, other_left, other_right, _) in spans[first..]
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

    #[test]
    fn only_moved_sides_join_runs_on_a_row() {
        let sides = |runs: &[SelectionRun], origin: f32, cw: f32| {
            let mut quads = Vec::new();
            append_outline(
                &mut quads,
                runs,
                (origin, 0.0),
                (cw, 16.0),
                1.0,
                Rgb::new(255, 255, 255),
            );
            quads
                .iter()
                .filter(|quad| quad.size[1] > 1.0)
                .map(|quad| quad.pos[0])
                .collect::<Vec<_>>()
        };
        // Fills meeting on a half pixel: the sides round the same x the
        // fills share, so the runs join.
        let met = [
            SelectionRun {
                row: 0,
                columns: 7..9,
                moved: [None, Some(74.5)],
            },
            SelectionRun {
                row: 0,
                columns: 10..11,
                moved: [Some(74.5), None],
            },
        ];
        assert_eq!(sides(&met, 0.0, 8.1), [57.0, 88.0]);
        // Runs that did not move keep both their sides even where the cell
        // between them rounds away.
        let apart = [
            SelectionRun {
                row: 0,
                columns: 0..4,
                moved: [None; 2],
            },
            SelectionRun {
                row: 0,
                columns: 5..9,
                moved: [None; 2],
            },
        ];
        assert_eq!(sides(&apart, 0.4, 0.6).len(), 4);
    }

    /// The outline of `runs` (row, columns, reach in 1x pixels) paints
    /// exactly the union's edge pixels, at 1x and 2x.
    fn traces_the_union(runs: &[(i32, Range<usize>, [i32; 2])]) {
        for scale in [1, 2] {
            let (cw, ch, ox, oy) = (10 * scale, 12 * scale, 3 * scale, 5 * scale);
            // Each run's pixel span, and the run as the renderer passes it.
            let runs: Vec<_> = runs
                .iter()
                .map(|(row, columns, reach)| {
                    let left = ox + columns.start as i32 * cw - reach[0] * scale;
                    let right = ox + columns.end as i32 * cw + reach[1] * scale;
                    let run = SelectionRun {
                        row: *row,
                        columns: columns.clone(),
                        moved: [
                            (reach[0] > 0).then_some(left as f32),
                            (reach[1] > 0).then_some(right as f32),
                        ],
                    };
                    (left, right, run)
                })
                .collect();
            let mut selected = HashSet::new();
            for (left, right, run) in &runs {
                let (left, right) = (*left, *right);
                for y in oy + run.row * ch..oy + (run.row + 1) * ch {
                    for x in left..right {
                        selected.insert((x, y));
                    }
                }
            }
            let runs: Vec<_> = runs.into_iter().map(|(_, _, run)| run).collect();
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

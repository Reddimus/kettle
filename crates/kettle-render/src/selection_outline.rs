use std::ops::Range;

use crate::{QuadInstance, Rgb, rect};

pub(crate) struct SelectionRun {
    pub row: i32,
    pub columns: Range<usize>,
}

/// Draw only the exposed edges of the union, inside the selection. Shared
/// row boundaries have no stroke, including the overlapping part of a step.
pub(crate) fn append_outline(
    target: &mut Vec<QuadInstance>,
    runs: &[SelectionRun],
    origin: (f32, f32),
    cell: (f32, f32),
    scale: f32,
    ink: Rgb,
) {
    let thickness = scale.round().max(1.0);
    let x = |column: usize| (origin.0 + column as f32 * cell.0).round();
    let y = |row: i32| (origin.1 + row as f32 * cell.1).round();
    for run in runs {
        let (left, right) = (x(run.columns.start), x(run.columns.end));
        let (top, bottom) = (y(run.row), y(run.row + 1));
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
        for (neighbor, edge) in [(run.row - 1, top), (run.row + 1, bottom - thickness)] {
            let mut start = run.columns.start;
            let first = runs.partition_point(|other| other.row < neighbor);
            for other in runs[first..]
                .iter()
                .take_while(|other| other.row == neighbor)
            {
                let end = other.columns.start.min(run.columns.end);
                if end > start {
                    target.push(rect(x(start), edge, x(end) - x(start), thickness, ink, 1.0));
                }
                start = start.max(other.columns.end);
                if start >= run.columns.end {
                    break;
                }
            }
            if start < run.columns.end {
                target.push(rect(x(start), edge, right - x(start), thickness, ink, 1.0));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn c09_outline_follows_exposed_union_edges_at_native_scales() {
        let runs = [
            SelectionRun {
                row: 0,
                columns: 1..5,
            },
            SelectionRun {
                row: 1,
                columns: 0..5,
            },
            SelectionRun {
                row: 2,
                columns: 0..3,
            },
            SelectionRun {
                row: 3,
                columns: 0..1,
            },
            SelectionRun {
                row: 3,
                columns: 2..3,
            },
        ];
        for scale in [1, 2] {
            let (cw, ch, ox, oy) = (10 * scale, 12 * scale, 3 * scale, 5 * scale);
            let mut selected = HashSet::new();
            for run in &runs {
                for y in oy + run.row * ch..oy + (run.row + 1) * ch {
                    for x in ox + run.columns.start as i32 * cw..ox + run.columns.end as i32 * cw {
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

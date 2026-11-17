//! Reciprocal Arabic seams. The visual-left cell owns a seam; its two
//! fragments use their own cell's face and colour and never cross the edge.

use std::collections::HashMap;

use glyphon::cosmic_text::{Color, LayoutGlyph};
use kettle_core::bidi::Joins;

use crate::batching::GlyphSource;
use crate::geometry::GridGlyphCell;
use crate::glyphpipe::{GlyphInstance, GlyphSlot};

#[derive(Default)]
pub(crate) struct JoinedRow {
    cells: HashMap<usize, JoinedCell>,
    ends: HashMap<usize, usize>,
    order: Vec<usize>,
}

struct JoinedCell {
    cell: GridGlyphCell,
    base_byte: usize,
    left: f32,
    right: f32,
    advance: f32,
    base: Option<Base>,
    images: Vec<(usize, f32)>,
    shift: f32,
}

struct Base {
    pen: f32,
    y: f32,
    color: Color,
    connector: Option<GlyphSlot>,
}

pub(crate) fn join_shift(joins: Joins, advance: f32, width: f32) -> f32 {
    let slack = width - advance;
    match (joins.left, joins.right) {
        (true, false) if slack > 0.0 => slack,
        (false, true) if slack < 0.0 => slack,
        (true, true) if slack < 0.0 => slack / 2.0,
        _ => 0.0,
    }
}

impl JoinedRow {
    pub fn add(&mut self, cell: GridGlyphCell, base_byte: usize, origin: f32, cw: f32) {
        if self.cells.contains_key(&cell.column) {
            return;
        }
        self.order.push(cell.column);
        self.ends.insert(cell.column + cell.width, cell.column);
        self.cells.insert(
            cell.column,
            JoinedCell {
                cell,
                base_byte,
                left: origin + cell.column as f32 * cw,
                right: origin + (cell.column + cell.width) as f32 * cw,
                advance: 0.0,
                base: None,
                images: Vec::new(),
                shift: 0.0,
            },
        );
    }

    /// Called only for glyphs carrying join bits, during the normal emit.
    /// A mark can share the base's cluster range, but never its positive advance.
    #[allow(clippy::too_many_arguments)]
    pub fn observe(
        &mut self,
        glyph: &LayoutGlyph,
        pen: f32,
        y: f32,
        slot: Option<GlyphSlot>,
        key: glyphon::cosmic_text::CacheKey,
        color: Color,
        image: Option<usize>,
        source: &mut impl GlyphSource,
    ) {
        let Some(cell) = GridGlyphCell::from_metadata(glyph.metadata)
            .and_then(|cell| self.cells.get_mut(&cell.column))
        else {
            return;
        };
        cell.advance += glyph.w;
        if let Some(image) = image {
            cell.images.push((image, pen));
        }
        if cell.base.is_none()
            && glyph.w > 0.0
            && glyph.glyph_id != 0
            && glyph.start <= cell.base_byte
            && cell.base_byte < glyph.end
            && slot.is_some()
        {
            cell.base = Some(Base {
                pen,
                y,
                color,
                connector: source.connector(key),
            });
        }
    }

    pub fn needs(&self, column: usize, target: usize) -> bool {
        column == target
            || self.cells.get(&target).is_some_and(|cell| {
                column == target + cell.cell.width || self.ends.get(&target) == Some(&column)
            })
    }

    fn sides(&self, cell: &JoinedCell) -> Joins {
        let drawable = |other: &JoinedCell| other.base.is_some();
        Joins {
            left: cell.cell.joins.left
                && self
                    .ends
                    .get(&cell.cell.column)
                    .and_then(|col| self.cells.get(col))
                    .is_some_and(|other| other.cell.joins.right && drawable(other)),
            right: cell.cell.joins.right
                && self
                    .cells
                    .get(&(cell.cell.column + cell.cell.width))
                    .is_some_and(|other| other.cell.joins.left && drawable(other)),
        }
    }

    /// Finalize only the collected joining images, never a second glyph walk.
    /// The normal emit rasterizes at integer pens, so this translation keeps
    /// the raster key (including its zero X subpixel bin) unchanged.
    pub fn finish(mut self, out: &mut Vec<GlyphInstance>, only: Option<usize>) {
        let shifts: Vec<_> = self
            .cells
            .iter()
            .map(|(&col, cell)| {
                let shift = if cell
                    .base
                    .as_ref()
                    .is_some_and(|base| base.connector.is_some())
                {
                    join_shift(self.sides(cell), cell.advance, cell.right - cell.left)
                } else {
                    0.0
                };
                (col, shift)
            })
            .collect();
        for (col, shift) in shifts {
            let cell = self.cells.get_mut(&col).unwrap();
            cell.shift = shift;
            for &(index, pen) in &cell.images {
                out[index].pos[0] += (pen + shift).round() - pen.round();
            }
        }
        for column in &self.order {
            let cell = &self.cells[column];
            if !cell.cell.joins.right || cell.base.is_none() {
                continue;
            }
            let Some(right) = self.cells.get(&(cell.cell.column + cell.cell.width)) else {
                continue;
            };
            if !right.cell.joins.left || right.base.is_none() {
                continue;
            }
            // Only the left cell visits this seam. Each fragment is clipped
            // to its own cell even when colours happen to match.
            cell.bridge(out, false, only);
            right.bridge(out, true, only);
        }
    }
}

impl JoinedCell {
    fn bridge(&self, out: &mut Vec<GlyphInstance>, left: bool, only: Option<usize>) {
        if only.is_some_and(|column| column != self.cell.column) {
            return;
        }
        let Some(base) = &self.base else { return };
        let Some(slot) = base.connector else { return };
        let pen = (base.pen + self.shift).round();
        let (x0, x1) = if left {
            if pen - self.left.round() < 0.5 {
                return;
            }
            (self.left.round(), (pen + 1.0).min(self.right.round()))
        } else {
            if self.right.round() - (pen + self.advance) < 0.5 {
                return;
            }
            (
                (pen + self.advance).floor().max(self.left.round() + 1.0) - 1.0,
                self.right.round(),
            )
        };
        if x1 <= x0 {
            return;
        }
        let color = base.color;
        out.push(GlyphInstance {
            pos: [x0, base.y - slot.top as f32],
            size: [x1 - x0, slot.h],
            uv: [slot.atlas_x + (slot.w / 2.0).floor(), slot.atlas_y],
            uv_size: [1.0, slot.h],
            color: [
                color.r() as f32 / 255.0,
                color.g() as f32 / 255.0,
                color.b() as f32 / 255.0,
                color.a() as f32 / 255.0,
            ],
            kind: slot.kind,
            _pad: [0; 1],
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use glyphon::cosmic_text::{CacheKey, CacheKeyFlags};

    const LEFT: Joins = Joins {
        left: true,
        right: false,
    };
    const RIGHT: Joins = Joins {
        left: false,
        right: true,
    };
    const BOTH: Joins = Joins {
        left: true,
        right: true,
    };

    fn cell(column: usize, width: usize, joins: Joins) -> GridGlyphCell {
        GridGlyphCell {
            column,
            width,
            joins,
            clustered: false,
            text_presentation: false,
            single_image: false,
        }
    }

    fn glyph(cell: GridGlyphCell, advance: f32) -> LayoutGlyph {
        LayoutGlyph {
            start: cell.column,
            end: cell.column + 1,
            font_size: 10.0,
            font_weight: glyphon::fontdb::Weight::NORMAL,
            line_height_opt: None,
            font_id: glyphon::fontdb::ID::dummy(),
            glyph_id: 7,
            x: 0.0,
            y: 0.0,
            w: advance,
            level: unicode_bidi::Level::ltr(),
            x_offset: 0.0,
            y_offset: 0.0,
            color_opt: None,
            metadata: cell.metadata(),
            cache_key_flags: CacheKeyFlags::empty(),
        }
    }

    fn slot() -> GlyphSlot {
        GlyphSlot {
            kind: 1,
            atlas_x: 100.0,
            atlas_y: 200.0,
            w: 5.0,
            h: 2.0,
            left: 0,
            top: 2,
        }
    }

    #[derive(Default)]
    struct Source {
        calls: Vec<CacheKey>,
        missing: bool,
    }
    impl GlyphSource for Source {
        fn slot(&mut self, _: CacheKey) -> Option<GlyphSlot> {
            Some(slot())
        }
        fn connector(&mut self, key: CacheKey) -> Option<GlyphSlot> {
            self.calls.push(key);
            (!self.missing).then(slot)
        }
    }

    fn observe(
        row: &mut JoinedRow,
        out: &mut Vec<GlyphInstance>,
        source: &mut Source,
        glyph: &LayoutGlyph,
        pen: f32,
        color: Color,
        drawable: bool,
    ) {
        let image = drawable.then_some(out.len());
        if drawable {
            out.push(GlyphInstance {
                pos: [pen.round(), 20.0],
                size: [4.0, 5.0],
                uv: [0.0; 2],
                uv_size: [4.0, 5.0],
                color: [1.0; 4],
                kind: 1,
                _pad: [0],
            });
        }
        row.observe(
            glyph,
            pen,
            glyph.physical((0.0, 20.0), 1.0).y as f32,
            drawable.then(slot),
            glyph.physical((0.0, 0.0), 1.0).cache_key,
            color,
            image,
            source,
        );
    }

    #[test]
    fn join_shift_puts_slack_on_the_joining_side() {
        for (joins, narrow, wide) in [
            (LEFT, 6.0, 0.0),
            (RIGHT, 0.0, -4.0),
            (BOTH, 0.0, -2.0),
            (Joins::default(), 0.0, 0.0),
        ] {
            assert_eq!(join_shift(joins, 4.0, 10.0), narrow);
            assert_eq!(join_shift(joins, 14.0, 10.0), wide);
        }
    }

    #[test]
    fn bridges_span_the_gap_once_and_keep_each_cells_colour() {
        let mut row = JoinedRow::default();
        let mut source = Source::default();
        let mut out = Vec::new();
        let a = cell(0, 1, RIGHT);
        let b = cell(1, 1, LEFT);
        row.add(a, 0, 0.0, 10.0);
        row.add(b, 1, 0.0, 10.0);
        // Visit RTL logical order, opposite to the seam's ownership order.
        observe(
            &mut row,
            &mut out,
            &mut source,
            &glyph(b, 4.0),
            10.0,
            Color::rgb(0, 0, 255),
            true,
        );
        observe(
            &mut row,
            &mut out,
            &mut source,
            &glyph(a, 4.0),
            0.0,
            Color::rgb(255, 0, 0),
            true,
        );
        row.finish(&mut out, None);
        assert_eq!(
            out.len(),
            4,
            "two glyphs and exactly two disjoint seam fragments"
        );
        assert_eq!(out[0].pos[0], 16.0);
        assert_eq!(out[1].pos[0], 0.0);
        assert_eq!(out[2].pos, [3.0, 18.0]);
        assert_eq!(out[2].size, [7.0, 2.0]);
        assert_eq!(out[2].color, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(out[3].pos, [10.0, 18.0]);
        assert_eq!(out[3].size, [7.0, 2.0]);
        assert_eq!(out[3].color, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(out[2].uv, [102.0, 200.0]);
        assert_eq!(out[2].uv_size, [1.0, 2.0]);
    }

    #[test]
    fn only_reciprocal_drawable_visual_neighbours_get_bridges_or_shifts() {
        for (column, joins, drawable) in [
            (1, LEFT, true),
            (1, RIGHT, true),
            (2, LEFT, true),
            (1, LEFT, false),
        ] {
            let mut row = JoinedRow::default();
            let mut source = Source::default();
            let mut out = Vec::new();
            let a = cell(0, 1, RIGHT);
            let b = cell(column, 1, joins);
            row.add(a, 0, 0.0, 10.0);
            row.add(b, column, 0.0, 10.0);
            observe(
                &mut row,
                &mut out,
                &mut source,
                &glyph(a, 14.0),
                0.0,
                Color::rgb(0, 0, 0),
                true,
            );
            observe(
                &mut row,
                &mut out,
                &mut source,
                &glyph(b, 4.0),
                column as f32 * 10.0,
                Color::rgb(0, 0, 0),
                drawable,
            );
            row.finish(&mut out, None);
            if column == 1 && joins == LEFT && drawable {
                assert_eq!(out[0].pos[0], -4.0);
                assert_eq!(out[1].pos[0], 16.0);
                assert_eq!(out.len(), 3);
            } else {
                assert_eq!(out[0].pos[0], 0.0);
                assert_eq!(out.len(), 1 + usize::from(drawable));
            }
        }
        let mut row = JoinedRow::default();
        let mut out = Vec::new();
        let mut source = Source::default();
        let lone = cell(0, 1, LEFT);
        row.add(lone, 0, 0.0, 10.0);
        observe(
            &mut row,
            &mut out,
            &mut source,
            &glyph(lone, 4.0),
            0.0,
            Color::rgb(0, 0, 0),
            true,
        );
        row.finish(&mut out, None);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].pos[0], 0.0,
            "ZWJ may choose an initial form without creating a partner"
        );
    }

    #[test]
    fn wide_joined_cells_reach_their_outer_edge() {
        for scale in [1.0, 2.0] {
            let mut row = JoinedRow::default();
            let mut source = Source::default();
            let mut out = Vec::new();
            let a = cell(0, 2, RIGHT);
            let b = cell(2, 2, LEFT);
            row.add(a, 0, 0.25, 10.0 * scale);
            row.add(b, 2, 0.25, 10.0 * scale);
            observe(
                &mut row,
                &mut out,
                &mut source,
                &glyph(a, 4.0 * scale),
                0.25,
                Color::rgb(0, 0, 0),
                true,
            );
            observe(
                &mut row,
                &mut out,
                &mut source,
                &glyph(b, 4.0 * scale),
                0.25 + 20.0 * scale,
                Color::rgb(0, 0, 0),
                true,
            );
            row.finish(&mut out, None);
            assert_eq!(out[1].pos[0], 36.0 * scale);
            assert_eq!(out[2].pos[0] + out[2].size[0], 20.0 * scale);
            assert_eq!(out[3].pos[0], 20.0 * scale);
        }
    }

    #[test]
    fn a_missing_tatweel_keeps_the_original_glyph_positions() {
        let mut row = JoinedRow::default();
        let mut source = Source {
            missing: true,
            ..Default::default()
        };
        let mut out = Vec::new();
        for (column, joins) in [(0, RIGHT), (1, LEFT)] {
            let cell = cell(column, 1, joins);
            row.add(cell, column, 0.0, 10.0);
            observe(
                &mut row,
                &mut out,
                &mut source,
                &glyph(cell, 4.0),
                column as f32 * 10.0,
                Color::rgb(0, 0, 0),
                true,
            );
        }
        row.finish(&mut out, None);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].pos[0], 10.0);
    }

    #[test]
    fn marks_share_the_shift_but_never_supply_a_connector_face_or_baseline() {
        let mut row = JoinedRow::default();
        let mut source = Source::default();
        let mut out = Vec::new();
        for (column, joins) in [(0, RIGHT), (1, LEFT)] {
            let cell = cell(column, 1, joins);
            row.add(cell, column, 0.0, 10.0);
            let base = glyph(cell, 4.0);
            observe(
                &mut row,
                &mut out,
                &mut source,
                &base,
                column as f32 * 10.0,
                Color::rgb(255, 0, 0),
                true,
            );
            let mut mark = base.clone();
            mark.w = 0.0;
            mark.glyph_id = 99;
            mark.font_size = 8.0;
            mark.y_offset = 1.0;
            observe(
                &mut row,
                &mut out,
                &mut source,
                &mark,
                column as f32 * 10.0 + 2.0,
                Color::rgb(0, 255, 0),
                true,
            );
        }
        row.finish(&mut out, None);
        assert_eq!(source.calls.len(), 2);
        assert!(source.calls.iter().all(|key| key.glyph_id == 7));
        assert_eq!(out.len(), 6);
        assert_eq!((out[2].pos[0], out[3].pos[0]), (16.0, 18.0));
        for bridge in &out[4..] {
            assert_eq!(bridge.pos[1], 18.0);
            assert_eq!(bridge.color, [1.0, 0.0, 0.0, 1.0]);
        }
    }

    #[test]
    fn connector_queries_are_once_per_cell_even_with_many_marks() {
        for count in [1, 100, 1000] {
            let mut row = JoinedRow::default();
            let mut source = Source::default();
            let mut out = Vec::new();
            for column in 0..count {
                let cell = cell(column, 1, BOTH);
                row.add(cell, column, 0.0, 10.0);
                let base = glyph(cell, 4.0);
                observe(
                    &mut row,
                    &mut out,
                    &mut source,
                    &base,
                    column as f32 * 10.0,
                    Color::rgb(0, 0, 0),
                    true,
                );
                let mut mark = base;
                mark.w = 0.0;
                for _ in 0..32 {
                    observe(
                        &mut row,
                        &mut out,
                        &mut source,
                        &mark,
                        column as f32 * 10.0,
                        Color::rgb(0, 0, 0),
                        true,
                    );
                }
            }
            row.finish(&mut out, None);
            assert_eq!(source.calls.len(), count);
            assert!(out.len() <= count * 35);
        }
    }
}

#[cfg(test)]
mod order_tests {
    use super::*;

    #[test]
    fn unchanged_joined_cells_keep_a_stable_connector_order() {
        let render = || {
            let mut row = JoinedRow::default();
            for column in 0..12 {
                let cell = GridGlyphCell {
                    column,
                    width: 1,
                    joins: Joins {
                        left: true,
                        right: true,
                    },
                    clustered: false,
                    text_presentation: false,
                    single_image: false,
                };
                row.add(cell, column, 0.0, 10.0);
                let held = row.cells.get_mut(&column).unwrap();
                held.advance = 4.0;
                held.base = Some(Base {
                    pen: column as f32 * 10.0,
                    y: 20.0,
                    color: Color::rgb(0, 0, 0),
                    connector: Some(GlyphSlot {
                        kind: 1,
                        atlas_x: 0.0,
                        atlas_y: 0.0,
                        w: 5.0,
                        h: 2.0,
                        left: 0,
                        top: 2,
                    }),
                });
            }
            let mut out = Vec::new();
            row.finish(&mut out, None);
            out.into_iter()
                .map(|glyph| (glyph.pos, glyph.size))
                .collect::<Vec<_>>()
        };
        let expected = render();
        for _ in 0..16 {
            assert_eq!(render(), expected);
        }
    }
}

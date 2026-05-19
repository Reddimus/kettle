//! Kitty *Unicode placeholder* decoding (graphics protocol §"Unicode
//! placeholders", `kitty/docs/graphics-protocol.rst:555`).
//!
//! An image transmitted with a *virtual placement* (`U=1`) is displayed by
//! emitting the placeholder character `U+10EEEE` in normal text, with the
//! image id encoded in the cell's foreground color, an optional placement id
//! in the underline color, and the cell's `(row, column)` plus the most
//! significant image-id byte encoded as combining diacritics.
//!
//! This module is the pure, host-agnostic decoder for that text encoding:
//! the diacritic⇄number table, per-cell diacritic parsing, the image-id
//! reconstruction, and the left-inheritance algorithm for omitted
//! diacritics. Compositing resolved placeholder cells against the grid
//! happens in the renderer (next ROADMAP cycle); this layer is fully unit
//! tested in isolation.

/// The Private-Use placeholder code point (`IMAGE_PLACEHOLDER_CHAR`,
/// `kitty/kitty/data-types.h:132`).
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// Row/column diacritics, in encoding order: `DIACRITICS[n]` is the combining
/// mark that encodes the number `n`. Auto-extracted from kitty
/// `gen/rowcolumn-diacritics.txt` (297 entries, strictly ascending).
static DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A, 0x034B, 0x034C,
    0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365, 0x0366, 0x0367, 0x0368, 0x0369,
    0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F, 0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592,
    0x0593, 0x0594, 0x0595, 0x0597, 0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1,
    0x05A8, 0x05A9, 0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6, 0x06D7, 0x06D8,
    0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2, 0x06E4, 0x06E7, 0x06E8, 0x06EB,
    0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736, 0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743,
    0x0745, 0x0747, 0x0749, 0x074A, 0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3,
    0x0816, 0x0817, 0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951, 0x0953, 0x0954,
    0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD, 0x193A, 0x1A17, 0x1A75, 0x1A76,
    0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C, 0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71,
    0x1B72, 0x1B73, 0x1CD0, 0x1CD1, 0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4,
    0x1DC5, 0x1DC6, 0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF, 0x1DE0, 0x1DE1,
    0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1, 0x20D4, 0x20D5, 0x20D6, 0x20D7,
    0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0, 0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2,
    0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6, 0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE,
    0x2DEF, 0x2DF0, 0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1, 0xA8E0, 0xA8E1,
    0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9, 0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED,
    0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2, 0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1,
    0xFE20, 0xFE21, 0xFE22, 0xFE23, 0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186,
    0x1D187, 0x1D188, 0x1D189, 0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

/// `true` if `c` is the Unicode image placeholder.
#[inline]
pub fn is_placeholder(c: char) -> bool {
    c == PLACEHOLDER
}

/// The number a row/column diacritic encodes, or `None` if `c` is not in the
/// kitty diacritic table. Table is ascending, so binary search is `O(log n)`.
pub fn diacritic_value(c: char) -> Option<u16> {
    DIACRITICS.binary_search(&(c as u32)).ok().map(|i| i as u16)
}

/// Explicit diacritics on one placeholder cell, in spec order
/// `[row, column, most-significant-id-byte]`. `None` = omitted (inherited).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CellDiacritics {
    pub row: Option<u16>,
    pub col: Option<u16>,
    pub msb: Option<u16>,
}

impl CellDiacritics {
    /// Parse the combining marks that follow a placeholder code point.
    /// Up to three are significant (row, column, msb); unknown marks end
    /// parsing (a foreign combining char is not part of the encoding).
    pub fn parse(marks: &[char]) -> CellDiacritics {
        let mut out = CellDiacritics::default();
        let slots = [&mut out.row, &mut out.col, &mut out.msb];
        let mut it = marks.iter();
        for slot in slots {
            match it.next() {
                Some(&m) => match diacritic_value(m) {
                    Some(v) => *slot = Some(v),
                    None => break,
                },
                None => break,
            }
        }
        out
    }
}

/// Reconstruct the 32-bit image id from the cell foreground color and the
/// optional most-significant-byte diacritic. `fg` carries the low 24 bits
/// (a 256-color index uses only the low 8); `msb` is the high byte.
#[inline]
pub fn image_id(fg: u32, msb: Option<u16>) -> u32 {
    ((msb.unwrap_or(0) as u32 & 0xFF) << 24) | (fg & 0x00FF_FFFF)
}

/// One placeholder cell as seen on screen, before inheritance is applied.
#[derive(Debug, Clone, Copy)]
pub struct RawCell {
    /// Foreground color value (256-index or 24-bit RGB) = low image-id bits.
    pub fg: u32,
    /// Underline color = placement id (0 / absent ⇒ any placement).
    pub placement_id: u32,
    pub diacritics: CellDiacritics,
}

/// A placeholder cell after the omitted-diacritic inheritance algorithm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedCell {
    pub image_id: u32,
    pub placement_id: u32,
    pub row: u16,
    pub col: u16,
}

/// Resolve a left-to-right run of placeholder cells, applying kitty's
/// omitted-diacritic inheritance (graphics-protocol.rst:626): a cell with
/// fewer than three diacritics inherits row / column+1 / msb from the cell
/// to its left **iff** that neighbor shares the same foreground and
/// underline (placement) colors and the relevant prefix matches.
pub fn resolve_run(cells: &[RawCell]) -> Vec<ResolvedCell> {
    let mut out: Vec<ResolvedCell> = Vec::with_capacity(cells.len());
    let mut prev_msb: Option<u16> = None;
    for (i, c) in cells.iter().enumerate() {
        let left = out.last().copied();
        let same_neighbor =
            i > 0 && cells[i - 1].fg == c.fg && cells[i - 1].placement_id == c.placement_id;

        let row = match c.diacritics.row {
            Some(r) => r,
            None if same_neighbor => left.map(|l| l.row).unwrap_or(0),
            None => 0,
        };
        let col = match c.diacritics.col {
            Some(cc) => cc,
            None if same_neighbor && c.diacritics.row.is_none() => {
                left.map(|l| l.col + 1).unwrap_or(0)
            }
            None if same_neighbor => left.map(|l| l.col + 1).unwrap_or(0),
            None => 0,
        };
        let msb = c
            .diacritics
            .msb
            .or(if same_neighbor { prev_msb } else { None });
        prev_msb = msb;

        out.push(ResolvedCell {
            image_id: image_id(c.fg, msb),
            placement_id: c.placement_id,
            row,
            col,
        });
    }
    out
}

/// The source sub-rectangle of an `img_w × img_h` image that the
/// placeholder cell at `(row, col)` of a `pcols × prows` virtual placement
/// displays. The image is stretch-fit across the placement grid (the
/// producer is expected to size `pcols × prows` to the image's aspect ratio,
/// per the spec). Returns `None` if the cell lies outside the placement or
/// the rectangle is empty. Pure — fully unit tested.
pub fn tile_src_rect(
    img_w: u32,
    img_h: u32,
    pcols: u16,
    prows: u16,
    row: u16,
    col: u16,
) -> Option<(u32, u32, u32, u32)> {
    if pcols == 0 || prows == 0 || col >= pcols || row >= prows || img_w == 0 || img_h == 0 {
        return None;
    }
    let (pc, pr) = (pcols as u32, prows as u32);
    let (c, r) = (col as u32, row as u32);
    // Use exact pixel boundaries so adjacent tiles abut with no gap/overlap.
    let x0 = c * img_w / pc;
    let x1 = (c + 1) * img_w / pc;
    let y0 = r * img_h / pr;
    let y1 = (r + 1) * img_h / pr;
    let (w, h) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
    if w == 0 || h == 0 {
        None
    } else {
        Some((x0, y0, w, h))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diacritic_table_matches_spec_examples() {
        // From the spec: U+305 → 0, U+30D → 1, U+30E → 2.
        assert_eq!(diacritic_value('\u{0305}'), Some(0));
        assert_eq!(diacritic_value('\u{030D}'), Some(1));
        assert_eq!(diacritic_value('\u{030E}'), Some(2));
        // Full table, ascending and exactly 297 entries.
        assert_eq!(DIACRITICS.len(), 297);
        assert!(DIACRITICS.windows(2).all(|w| w[0] < w[1]));
        assert_eq!(diacritic_value(PLACEHOLDER), None);
        assert_eq!(diacritic_value('a'), None);
    }

    #[test]
    fn parse_cell_diacritics() {
        let d = CellDiacritics::parse(&['\u{030D}', '\u{0305}', '\u{030E}']);
        assert_eq!(
            d,
            CellDiacritics {
                row: Some(1),
                col: Some(0),
                msb: Some(2)
            }
        );
        // Omitted trailing diacritics stay None; a non-diacritic stops parse.
        let d2 = CellDiacritics::parse(&['\u{0305}']);
        assert_eq!(d2.row, Some(0));
        assert_eq!(d2.col, None);
        let d3 = CellDiacritics::parse(&['\u{0305}', 'x', '\u{030E}']);
        assert_eq!((d3.row, d3.col, d3.msb), (Some(0), None, None));
    }

    #[test]
    fn image_id_combines_msb_and_fg() {
        // Spec: 33554474 = 42 + (2 << 24), msb diacritic U+30E = 2.
        assert_eq!(image_id(42, Some(2)), 33_554_474);
        assert_eq!(image_id(42, None), 42);
        // High bits of fg beyond 24 are masked off.
        assert_eq!(image_id(0xFF00_002A, Some(2)), 33_554_474);
    }

    #[test]
    fn resolve_spec_2x2_grid() {
        // The spec's 2×2 placeholder for image id 42:
        //   row0: (0,0) (0,1)   row1: (1,0) (1,1)
        // Each cell here carries explicit row+col diacritics, fg = 42.
        let mk = |r: u16, c: u16| RawCell {
            fg: 42,
            placement_id: 0,
            diacritics: CellDiacritics {
                row: Some(r),
                col: Some(c),
                msb: None,
            },
        };
        let res = resolve_run(&[mk(0, 0), mk(0, 1), mk(1, 0), mk(1, 1)]);
        let coords: Vec<(u16, u16)> = res.iter().map(|c| (c.row, c.col)).collect();
        assert_eq!(coords, vec![(0, 0), (0, 1), (1, 0), (1, 1)]);
        assert!(res.iter().all(|c| c.image_id == 42));
    }

    #[test]
    fn resolve_inherits_omitted_column_from_left() {
        // First cell explicit (row 0, col 0); the next two omit everything
        // and share fg+placement → column auto-increments, row inherited.
        let explicit = RawCell {
            fg: 7,
            placement_id: 0,
            diacritics: CellDiacritics {
                row: Some(0),
                col: Some(0),
                msb: None,
            },
        };
        let inherit = RawCell {
            fg: 7,
            placement_id: 0,
            diacritics: CellDiacritics::default(),
        };
        let res = resolve_run(&[explicit, inherit, inherit]);
        assert_eq!(
            res.iter().map(|c| (c.row, c.col)).collect::<Vec<_>>(),
            vec![(0, 0), (0, 1), (0, 2)]
        );
        // A differing fg breaks inheritance (neighbor not "the same").
        let other = RawCell {
            fg: 9,
            placement_id: 0,
            diacritics: CellDiacritics::default(),
        };
        let res2 = resolve_run(&[explicit, other]);
        assert_eq!((res2[1].row, res2[1].col), (0, 0));
    }

    #[test]
    fn differing_placement_id_breaks_inheritance() {
        // Same fg/image but a different placement id (underline color) ⇒
        // the neighbor is not "the same", so nothing is inherited.
        let explicit = RawCell {
            fg: 7,
            placement_id: 1,
            diacritics: CellDiacritics {
                row: Some(3),
                col: Some(9),
                msb: None,
            },
        };
        let other_place = RawCell {
            fg: 7,
            placement_id: 2,
            diacritics: CellDiacritics::default(),
        };
        let res = resolve_run(&[explicit, other_place]);
        assert_eq!((res[0].row, res[0].col), (3, 9));
        assert_eq!(
            (res[1].row, res[1].col),
            (0, 0),
            "different placement id ⇒ no inheritance from the left"
        );
        assert_eq!(res[1].placement_id, 2);
    }

    #[test]
    fn tile_rects_tile_the_image_without_gaps() {
        // 100×40 image over a 2×2 placement: tiles must abut exactly and
        // cover the whole image.
        let r00 = tile_src_rect(100, 40, 2, 2, 0, 0).unwrap();
        let r01 = tile_src_rect(100, 40, 2, 2, 0, 1).unwrap();
        let r10 = tile_src_rect(100, 40, 2, 2, 1, 0).unwrap();
        assert_eq!(r00, (0, 0, 50, 20));
        assert_eq!(r01, (50, 0, 50, 20));
        assert_eq!(r10, (0, 20, 50, 20));
        // Right edge of col0 meets left edge of col1; covers full width.
        assert_eq!(r00.0 + r00.2, r01.0);
        assert_eq!(r01.0 + r01.2, 100);
        // Non-divisible sizes still tile exactly (no lost/duplicated pixels).
        let a = tile_src_rect(7, 1, 3, 1, 0, 0).unwrap();
        let b = tile_src_rect(7, 1, 3, 1, 0, 1).unwrap();
        let c = tile_src_rect(7, 1, 3, 1, 0, 2).unwrap();
        assert_eq!(a.0 + a.2, b.0);
        assert_eq!(b.0 + b.2, c.0);
        assert_eq!(c.0 + c.2, 7);
        // Out-of-placement / degenerate inputs → None.
        assert!(tile_src_rect(100, 40, 2, 2, 2, 0).is_none());
        assert!(tile_src_rect(100, 40, 0, 2, 0, 0).is_none());
        assert!(tile_src_rect(0, 40, 2, 2, 0, 0).is_none());
    }

    #[test]
    fn resolve_inherits_msb() {
        let head = RawCell {
            fg: 42,
            placement_id: 0,
            diacritics: CellDiacritics {
                row: Some(0),
                col: Some(0),
                msb: Some(2),
            },
        };
        let tail = RawCell {
            fg: 42,
            placement_id: 0,
            diacritics: CellDiacritics::default(),
        };
        let res = resolve_run(&[head, tail]);
        assert_eq!(res[0].image_id, 33_554_474);
        assert_eq!(res[1].image_id, 33_554_474, "msb inherited from left");
    }
}

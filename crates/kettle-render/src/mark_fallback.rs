//! Fallback placement for combining marks their shaping font left unplaced.
//!
//! harfrust positions a mark from the font's GPOS mark attachments. Like
//! HarfBuzz, it places marks by combining class only when the font has no
//! GPOS table at all, so a font that has one but no anchor for a pair leaves
//! the mark where its outline sits. On macOS the bundled JetBrains Mono lacks
//! U+0324, so `Z\u{324}\u{308}` is shaped in San Francisco, whose GPOS only
//! kerns: its diaeresis stays at lowercase height inside the capital, and
//! both marks hang from the base's right edge.
//!
//! [`line_shifts`] finds those marks once per shaped row and places them as
//! hb-ot-shape-fallback does: above classes stack upward from the top of the
//! base's ink, below classes stack downward from its bottom, each with a gap
//! of 1/16 em, centred on (or aligned to) the base's ink. A mark the font
//! attaches, or one the shaper already moved, keeps its position. The grid
//! emit applies the cached shifts through [`MarkShift::moved`] and
//! [`MarkShift::raise_px`].

use glyphon::cosmic_text::{CacheKeyFlags, Command, FontSystem, LayoutGlyph, SwashCache};
use read_fonts::TableProvider;
use read_fonts::tables::gpos::PositionSubtables;
use read_fonts::types::GlyphId16;

/// The gap HarfBuzz's fallback leaves between a mark and what it sits on.
const GAP_EM: f32 = 1.0 / 16.0;

/// How far past its line a fallback mark may reach, in lines: about the
/// descender zone of the row above, so a capital keeps two stacked marks
/// while the neighbouring row's text stays legible. A stack that does not
/// fit is held at that edge, and a mark the font drew further out is not
/// moved further.
const OVERFLOW_LINES: f32 = 0.25;

/// One mark moved by fallback placement.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct MarkShift {
    /// The glyph's index in its laid-out line.
    pub(crate) index: u32,
    /// The glyph the shift was computed for. A shift whose glyph no longer
    /// matches is ignored.
    pub(crate) glyph_id: u16,
    /// The offsets, in ems of the glyph's font size, `y` upward.
    pub(crate) x: f32,
    pub(crate) y: f32,
    /// The placed ink's top and bottom, in ems above the baseline.
    pub(crate) top: f32,
    pub(crate) bottom: f32,
    /// Whether the mark sits above its base, which sets the rounding.
    pub(crate) above: bool,
}

impl MarkShift {
    /// The glyph moved sideways by the shift. Its vertical move is applied
    /// to the rasterized image, in whole pixels, by [`Self::raise_px`].
    pub(crate) fn moved(&self, glyph: &LayoutGlyph) -> LayoutGlyph {
        LayoutGlyph {
            x_offset: glyph.x_offset + self.x,
            ..glyph.clone()
        }
    }

    /// Whole pixels to raise the glyph's image by, for a line whose box is
    /// `line_top..line_top + line_height` with its baseline at `line_y`.
    ///
    /// Rounded away from the base, so the gap never shrinks below what was
    /// computed, and limited to [`OVERFLOW_LINES`] past the line box.
    pub(crate) fn raise_px(&self, size: f32, line_top: f32, line_height: f32, line_y: f32) -> i32 {
        let mut raise = self.y * size;
        let reach = line_height * OVERFLOW_LINES;
        if self.above {
            let ceiling = line_y - line_top + reach;
            let over = self.top * size - ceiling;
            if over > 0.0 {
                raise = (raise - over).max(raise.min(0.0));
            }
            raise.ceil() as i32
        } else {
            let floor = line_y - line_top - line_height - reach;
            let under = floor - self.bottom * size;
            if under > 0.0 {
                raise = (raise + under).min(raise.max(0.0));
            }
            raise.floor() as i32
        }
    }
}

/// The shift for the glyph at `index` with id `glyph_id`, if any. `next`
/// walks `shifts`, which is sorted by index, across one line's glyphs.
pub(crate) fn shift_for<'a>(
    shifts: &'a [MarkShift],
    next: &mut usize,
    index: usize,
    glyph_id: u16,
) -> Option<&'a MarkShift> {
    while shifts
        .get(*next)
        .is_some_and(|shift| (shift.index as usize) < index)
    {
        *next += 1;
    }
    shifts
        .get(*next)
        .filter(|shift| shift.index as usize == index && shift.glyph_id == glyph_id)
}

/// The fallback shifts of every line of `buffer`, by line.
pub(crate) fn buffer_shifts(
    buffer: &glyphon::Buffer,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
) -> Vec<Box<[MarkShift]>> {
    buffer
        .lines
        .iter()
        .map(
            |line| match line.layout_opt().and_then(|layout| layout.first()) {
                Some(layout) => line_shifts(&layout.glyphs, line.text(), fonts, swash),
                None => Box::default(),
            },
        )
        .collect()
}

/// The fallback shifts of one laid-out line of `text`.
pub(crate) fn line_shifts(
    glyphs: &[LayoutGlyph],
    text: &str,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
) -> Box<[MarkShift]> {
    let mut shifts = Vec::new();
    let mut first = 0;
    for cluster in glyphs.chunk_by(|a, b| (a.start, a.end) == (b.start, b.end)) {
        let start = first;
        first += cluster.len();
        // Only a cluster with a zero-advance glyph after its first carries a
        // mark; everything else costs this one test.
        if !cluster.iter().skip(1).any(|glyph| glyph.w == 0.0) {
            continue;
        }
        let Some(cluster_text) = text.get(cluster[0].start..cluster[0].end) else {
            continue;
        };
        // Emoji clusters fit their combined ink to the cell instead.
        if super::fonts::emoji_text(cluster_text) {
            continue;
        }
        place_cluster(cluster, start, cluster_text, fonts, swash, &mut shifts);
    }
    shifts.into_boxed_slice()
}

/// Ink bounds, in pixels, `y` upward.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Ink {
    pub(crate) left: f32,
    pub(crate) right: f32,
    pub(crate) bottom: f32,
    pub(crate) top: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Align {
    Left,
    Centre,
    Right,
    /// A double diacritic, centred on the end of its base's advance so it
    /// spans into the next cell.
    Spanning,
}

/// Where a canonical combining class puts its mark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MarkClass {
    above: bool,
    align: Align,
    /// Attached classes touch their base: no gap.
    attached: bool,
}

/// The placement of canonical combining class `ccc`, for the positional
/// classes hb-ot-shape-fallback places. Overlays (1), nukta (7), virama (9),
/// the fixed-position classes (10..=199) and the left and right classes
/// (224, 226) stay where the font drew them.
pub(crate) fn mark_class(ccc: u8) -> Option<MarkClass> {
    let (above, align, attached) = match ccc {
        200 => (false, Align::Left, true),
        202 => (false, Align::Centre, true),
        214 => (true, Align::Centre, true),
        216 => (true, Align::Right, true),
        218 => (false, Align::Left, false),
        220 => (false, Align::Centre, false),
        222 => (false, Align::Right, false),
        228 => (true, Align::Left, false),
        230 => (true, Align::Centre, false),
        232 => (true, Align::Right, false),
        233 => (false, Align::Spanning, false),
        234 => (true, Align::Spanning, false),
        _ => return None,
    };
    Some(MarkClass {
        above,
        align,
        attached,
    })
}

/// Places `mark` on `stack`, the base's ink grown by the marks already
/// placed in this class, and grows the stack by it. Returns the move.
///
/// This is hb-ot-shape-fallback's `position_mark`, aligned on the base's ink
/// rather than its advance: a below mark is never moved up, and an above
/// mark that sits higher than it needs to moves only half the way down.
pub(crate) fn place(
    class: MarkClass,
    mark: Ink,
    base: Ink,
    base_end: f32,
    stack: &mut Ink,
    gap: f32,
) -> (f32, f32) {
    let dx = match class.align {
        Align::Left => base.left - mark.left,
        Align::Right => base.right - mark.right,
        Align::Centre => (base.left + base.right - mark.left - mark.right) / 2.0,
        Align::Spanning => base_end - (mark.left + mark.right) / 2.0,
    };
    let gap = if class.attached { 0.0 } else { gap };
    let dy = if class.above {
        let dy = stack.top + gap - mark.bottom;
        let dy = if dy < 0.0 { dy / 2.0 } else { dy };
        stack.top = mark.top + dy;
        dy
    } else {
        let dy = (stack.bottom - gap - mark.top).min(0.0);
        stack.bottom = mark.bottom + dy;
        dy
    };
    (dx, dy)
}

/// How the shaping font placed one mark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Attachment {
    /// No anchor joins the mark to its base or an earlier mark.
    None,
    /// Anchored to its base, or the table could not be read: it stays.
    Base,
    /// Anchored to the earlier mark at this index in the cluster.
    Mark(usize),
}

fn place_cluster(
    cluster: &[LayoutGlyph],
    start: usize,
    text: &str,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    shifts: &mut Vec<MarkShift>,
) {
    let ccc = icu_normalizer::properties::CanonicalCombiningClassMapBorrowed::new();
    // The shaper decomposes a precomposed scalar the font lacks, so its marks
    // are looked for in the canonical decomposition.
    let mut marks: Vec<(char, u8, bool)> = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd()
        .normalize_iter(text.chars())
        .filter_map(|c| {
            let class = ccc.get_u8(c);
            (class != 0).then_some((c, class, false))
        })
        .collect();
    if marks.is_empty() {
        return;
    }
    let mut base: Option<(usize, Ink, f32)> = None;
    let mut stack = Ink {
        left: 0.0,
        right: 0.0,
        bottom: 0.0,
        top: 0.0,
    };
    let mut last_class = None;
    // Each glyph's shift in this cluster, for marks anchored to a moved mark.
    let mut moved: Vec<Option<(f32, f32)>> = vec![None; cluster.len()];
    for (i, glyph) in cluster.iter().enumerate() {
        if i == 0 || glyph.w != 0.0 {
            // A base: the cluster's first glyph or one with an advance (a
            // ligature component). A base without ink, such as a space,
            // keeps the marks drawn on it.
            base = ink(glyph, fonts, swash).map(|ink| (i, ink, pen(glyph).0 + glyph.w));
            if let Some((_, ink, _)) = base {
                stack = ink;
            }
            last_class = None;
            continue;
        }
        let Some((base_index, base_ink, base_end)) = base else {
            continue;
        };
        let Some(font) = fonts.get_font(glyph.font_id, glyph.font_weight) else {
            continue;
        };
        // The mark's scalar: the first unclaimed one the font maps to this
        // glyph. A glyph the font substituted maps to none and stays.
        let charmap = font.as_swash().charmap();
        let Some(slot) = marks
            .iter()
            .position(|&(c, _, claimed)| !claimed && charmap.map(c) == glyph.glyph_id)
        else {
            continue;
        };
        marks[slot].2 = true;
        let class_id = marks[slot].1;
        if last_class != Some(class_id) {
            stack = base_ink;
            last_class = Some(class_id);
        }
        let Some(class) = mark_class(class_id) else {
            continue;
        };
        let Some(mark_ink) = ink(glyph, fonts, swash) else {
            continue;
        };
        let size = glyph.font_size;
        // A mark the shaper moved stays: an anchor, a kern, or harfrust's own
        // fallback for a font without GPOS. It is looked up only when it may
        // be anchored to a mark this placement moved.
        let shaper_moved = glyph.x_offset != 0.0 || glyph.y_offset != 0.0;
        let attachment = if shaper_moved && moved.iter().all(Option::is_none) {
            Attachment::Base
        } else {
            let Some(index) = fonts.db().face(glyph.font_id).map(|face| face.index) else {
                continue;
            };
            let base_glyph = &cluster[base_index];
            let earlier: Vec<(usize, u16)> = (base_index + 1..i)
                .rev()
                .filter(|&j| cluster[j].font_id == glyph.font_id)
                .map(|j| (j, cluster[j].glyph_id))
                .collect();
            gpos_attachment(
                font.data(),
                index,
                (base_glyph.font_id == glyph.font_id).then_some(base_glyph.glyph_id),
                glyph.glyph_id,
                &earlier,
            )
        };
        let (dx, dy) = match attachment {
            Attachment::None if !shaper_moved => place(
                class,
                mark_ink,
                base_ink,
                base_end,
                &mut stack,
                size * GAP_EM,
            ),
            // Anchored to a mark this placement moved: it moves with it.
            Attachment::Mark(j) if moved[j].is_some() => {
                let (dx, dy) = moved[j].unwrap_or_default();
                grow(&mut stack, class, mark_ink, dy);
                (dx, dy)
            }
            Attachment::None | Attachment::Base | Attachment::Mark(_) => {
                grow(&mut stack, class, mark_ink, 0.0);
                continue;
            }
        };
        moved[i] = Some((dx, dy));
        shifts.push(MarkShift {
            index: (start + i) as u32,
            glyph_id: glyph.glyph_id,
            x: dx / size,
            y: dy / size,
            top: (mark_ink.top + dy) / size,
            bottom: (mark_ink.bottom + dy) / size,
            above: class.above,
        });
    }
}

/// Grows `stack` by a mark that keeps its place (raised by `dy`), so a later
/// fallback mark of its class stacks beyond it.
fn grow(stack: &mut Ink, class: MarkClass, mark: Ink, dy: f32) {
    if class.above {
        stack.top = stack.top.max(mark.top + dy);
    } else {
        stack.bottom = stack.bottom.min(mark.bottom + dy);
    }
}

/// The glyph's pen in its line, in pixels, `y` upward from the baseline.
fn pen(glyph: &LayoutGlyph) -> (f32, f32) {
    (
        glyph.x + glyph.x_offset * glyph.font_size,
        glyph.y_offset * glyph.font_size - glyph.y,
    )
}

/// The glyph's outline bounds in its line, or `None` for no outline. Hinting
/// is off so the bounds scale with the font size.
fn ink(glyph: &LayoutGlyph, fonts: &mut FontSystem, swash: &mut SwashCache) -> Option<Ink> {
    let mut key = glyph.physical((0.0, 0.0), 1.0).cache_key;
    key.flags |= CacheKeyFlags::DISABLE_HINTING;
    let commands = swash.get_outline_commands_uncached(fonts, key)?;
    let mut bounds: Option<Ink> = None;
    let mut add = |x: f32, y: f32| {
        let ink = bounds.get_or_insert(Ink {
            left: x,
            right: x,
            bottom: y,
            top: y,
        });
        ink.left = ink.left.min(x);
        ink.right = ink.right.max(x);
        ink.bottom = ink.bottom.min(y);
        ink.top = ink.top.max(y);
    };
    for command in commands.iter() {
        match *command {
            Command::MoveTo(p) | Command::LineTo(p) => add(p.x, p.y),
            Command::QuadTo(a, p) => {
                add(a.x, a.y);
                add(p.x, p.y);
            }
            Command::CurveTo(a, b, p) => {
                add(a.x, a.y);
                add(b.x, b.y);
                add(p.x, p.y);
            }
            Command::Close => {}
        }
    }
    let ink = bounds.filter(|ink| ink.right > ink.left && ink.top > ink.bottom)?;
    let (x, y) = pen(glyph);
    Some(Ink {
        left: ink.left + x,
        right: ink.right + x,
        bottom: ink.bottom + y,
        top: ink.top + y,
    })
}

/// Whether the font's GPOS anchors `mark` to `base` (mark-to-base or
/// mark-to-ligature) or to one of `earlier`, the cluster's preceding marks
/// nearest first (mark-to-mark).
///
/// Every mark lookup counts, whatever feature or script enables it: when in
/// doubt the mark keeps the font's placement.
fn gpos_attachment(
    data: &[u8],
    index: u32,
    base: Option<u16>,
    mark: u16,
    earlier: &[(usize, u16)],
) -> Attachment {
    let Ok(font) = read_fonts::FontRef::from_index(data, index) else {
        return Attachment::Base;
    };
    let gpos = match font.gpos() {
        Ok(gpos) => gpos,
        Err(read_fonts::ReadError::TableIsMissing(_)) => return Attachment::None,
        Err(_) => return Attachment::Base,
    };
    match anchored(&gpos, base, mark, earlier) {
        Ok(attachment) => attachment,
        Err(_) => Attachment::Base,
    }
}

fn anchored(
    gpos: &read_fonts::tables::gpos::Gpos<'_>,
    base: Option<u16>,
    mark: u16,
    earlier: &[(usize, u16)],
) -> Result<Attachment, read_fonts::ReadError> {
    type Anchors =
        [read_fonts::types::BigEndian<read_fonts::types::Nullable<read_fonts::types::Offset16>>];
    let has = |anchors: &Anchors, class: u16| {
        anchors
            .get(usize::from(class))
            .is_some_and(|anchor| !anchor.get().is_null())
    };
    let mark = GlyphId16::new(mark);
    let base = base.map(GlyphId16::new);
    let mut to_base = false;
    // The nearest earlier mark with an anchor, as a position in `earlier`.
    // Mark-to-mark lookups run after mark-to-base ones, so it wins.
    let mut to_mark: Option<usize> = None;
    for lookup in gpos.lookup_list()?.lookups().iter() {
        match lookup?.subtables()? {
            PositionSubtables::MarkToBase(subtables) => {
                let Some(base) = base else { continue };
                for subtable in subtables.iter() {
                    let subtable = subtable?;
                    let (Some(mark_index), Some(base_index)) = (
                        subtable.mark_coverage()?.get(mark),
                        subtable.base_coverage()?.get(base),
                    ) else {
                        continue;
                    };
                    let class = mark_class_of(&subtable.mark_array()?, mark_index)?;
                    let record = subtable
                        .base_array()?
                        .base_records()
                        .get(usize::from(base_index))?;
                    to_base |= has(record.base_anchor_offsets(), class);
                }
            }
            PositionSubtables::MarkToLig(subtables) => {
                let Some(base) = base else { continue };
                for subtable in subtables.iter() {
                    let subtable = subtable?;
                    let (Some(mark_index), Some(ligature_index)) = (
                        subtable.mark_coverage()?.get(mark),
                        subtable.ligature_coverage()?.get(base),
                    ) else {
                        continue;
                    };
                    let class = mark_class_of(&subtable.mark_array()?, mark_index)?;
                    let attach = subtable
                        .ligature_array()?
                        .ligature_attaches()
                        .get(usize::from(ligature_index))?;
                    for component in attach.component_records().iter() {
                        to_base |= has(component?.ligature_anchor_offsets(), class);
                    }
                }
            }
            PositionSubtables::MarkToMark(subtables) => {
                for subtable in subtables.iter() {
                    let subtable = subtable?;
                    let Some(mark_index) = subtable.mark1_coverage()?.get(mark) else {
                        continue;
                    };
                    let class = mark_class_of(&subtable.mark1_array()?, mark_index)?;
                    let coverage = subtable.mark2_coverage()?;
                    let records = subtable.mark2_array()?.mark2_records();
                    let nearer = to_mark.unwrap_or(earlier.len());
                    for (position, &(_, previous)) in earlier[..nearer].iter().enumerate() {
                        let Some(previous_index) = coverage.get(GlyphId16::new(previous)) else {
                            continue;
                        };
                        let record = records.get(usize::from(previous_index))?;
                        if has(record.mark2_anchor_offsets(), class) {
                            to_mark = Some(position);
                            break;
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(match to_mark {
        Some(position) => Attachment::Mark(earlier[position].0),
        None if to_base => Attachment::Base,
        None => Attachment::None,
    })
}

fn mark_class_of(
    array: &read_fonts::tables::gpos::MarkArray<'_>,
    index: u16,
) -> Result<u16, read_fonts::ReadError> {
    array
        .mark_records()
        .get(usize::from(index))
        .map(read_fonts::tables::gpos::MarkRecord::mark_class)
        .ok_or(read_fonts::ReadError::OutOfBounds)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ink(left: f32, right: f32, bottom: f32, top: f32) -> Ink {
        Ink {
            left,
            right,
            bottom,
            top,
        }
    }

    #[test]
    fn positional_classes_place_and_the_rest_stay() {
        let above = mark_class(230).expect("above");
        assert!(above.above && above.align == Align::Centre && !above.attached);
        let below = mark_class(220).expect("below");
        assert!(!below.above && below.align == Align::Centre && !below.attached);
        assert!(mark_class(202).is_some_and(|class| class.attached && !class.above));
        assert!(mark_class(214).is_some_and(|class| class.attached && class.above));
        assert_eq!(mark_class(228).map(|class| class.align), Some(Align::Left));
        assert_eq!(mark_class(232).map(|class| class.align), Some(Align::Right));
        assert_eq!(
            mark_class(234).map(|class| class.align),
            Some(Align::Spanning)
        );
        // Overlay, nukta, virama, a Hebrew point, iota subscript, the left
        // and right classes, and a starter keep the font's placement.
        for ccc in [0, 1, 7, 9, 18, 224, 226, 240] {
            assert_eq!(mark_class(ccc), None, "class {ccc}");
        }
    }

    /// San Francisco's `Z\u{324}\u{308}` in pixels at 13 px: both marks
    /// centred on the base's right edge, the diaeresis inside the capital.
    #[test]
    fn marks_stack_out_from_the_base_ink_with_a_gap() {
        let base = ink(0.0, 8.0, 0.0, 10.0);
        let above = mark_class(230).unwrap();
        let below = mark_class(220).unwrap();

        let mut stack = base;
        let (dx, dy) = place(
            below,
            ink(6.0, 10.0, -3.0, -1.0),
            base,
            8.0,
            &mut stack,
            1.0,
        );
        assert_eq!((dx, dy), (-4.0, 0.0), "already clear below: only centred");
        assert_eq!(stack.bottom, -3.0);

        let mut stack = base;
        let (dx, dy) = place(above, ink(6.0, 10.0, 7.0, 9.0), base, 8.0, &mut stack, 1.0);
        assert_eq!((dx, dy), (-4.0, 4.0), "lifted to the cap top plus the gap");
        assert_eq!(stack.top, 13.0);
        // A second mark of the class stacks on the first.
        let (_, dy) = place(above, ink(6.0, 10.0, 7.0, 9.0), base, 8.0, &mut stack, 1.0);
        assert_eq!(dy, 7.0);
        assert_eq!(stack.top, 16.0);
    }

    #[test]
    fn below_marks_never_rise_and_above_marks_sink_halfway() {
        let base = ink(0.0, 8.0, 0.0, 10.0);
        let below = mark_class(220).unwrap();
        let above = mark_class(230).unwrap();

        // Overlapping the base's foot: pushed down to the gap.
        let mut stack = base;
        let (_, dy) = place(below, ink(3.0, 5.0, -1.0, 2.0), base, 8.0, &mut stack, 1.0);
        assert_eq!(dy, -3.0);
        assert_eq!(stack.bottom, -4.0);
        // Far below already: stays, and the stack grows from where it is.
        let mut stack = base;
        let (_, dy) = place(below, ink(3.0, 5.0, -8.0, -6.0), base, 8.0, &mut stack, 1.0);
        assert_eq!(dy, 0.0);
        assert_eq!(stack.bottom, -8.0);

        // Higher than it needs to be: moved half the way down.
        let mut stack = base;
        let (_, dy) = place(above, ink(3.0, 5.0, 15.0, 17.0), base, 8.0, &mut stack, 1.0);
        assert_eq!(dy, -2.0);
        assert_eq!(stack.top, 15.0);
    }

    #[test]
    fn attached_and_aligned_classes() {
        let base = ink(1.0, 7.0, 0.0, 10.0);
        let mark = ink(0.0, 2.0, 4.0, 6.0);

        let mut stack = base;
        let attached = mark_class(214).unwrap();
        assert_eq!(
            place(attached, mark, base, 8.0, &mut stack, 1.0),
            (3.0, 6.0),
            "an attached mark touches its base"
        );
        let mut stack = base;
        assert_eq!(
            place(mark_class(228).unwrap(), mark, base, 8.0, &mut stack, 1.0).0,
            1.0,
            "left edges meet"
        );
        let mut stack = base;
        assert_eq!(
            place(mark_class(232).unwrap(), mark, base, 8.0, &mut stack, 1.0).0,
            5.0,
            "right edges meet"
        );
        let mut stack = base;
        assert_eq!(
            place(mark_class(234).unwrap(), mark, base, 8.0, &mut stack, 1.0).0,
            7.0,
            "a double mark centres on the end of the advance"
        );
    }

    fn shift(y: f32, bottom: f32, top: f32, above: bool) -> MarkShift {
        MarkShift {
            index: 1,
            glyph_id: 7,
            x: 0.0,
            y,
            top,
            bottom,
            above,
        }
    }

    #[test]
    fn raises_round_away_from_the_base() {
        // A 10 px font on a 16 px line with its baseline 12 px down.
        let (size, top, height, baseline) = (10.0, 0.0, 16.0, 12.0);
        assert_eq!(
            shift(0.21, 0.8, 0.9, true).raise_px(size, top, height, baseline),
            3
        );
        assert_eq!(
            shift(-0.21, 0.8, 0.9, true).raise_px(size, top, height, baseline),
            -2
        );
        assert_eq!(
            shift(-0.21, -0.4, -0.3, false).raise_px(size, top, height, baseline),
            -3
        );
        assert_eq!(
            shift(0.0, -0.4, -0.3, false).raise_px(size, top, height, baseline),
            0
        );
    }

    #[test]
    fn a_tall_stack_stops_a_quarter_line_past_its_row() {
        // The row reaches 12 px above the baseline and 4 px below it; a stack
        // may reach 4 px further either way.
        let (size, top, height, baseline) = (10.0, 0.0, 16.0, 12.0);
        // Top at 18 px, 2 px past the 16 px ceiling: 5 px up becomes 3.
        assert_eq!(
            shift(0.5, 1.6, 1.8, true).raise_px(size, top, height, baseline),
            3
        );
        // Already past the ceiling as drawn: not moved further, not sunk.
        assert_eq!(
            shift(0.2, 1.8, 2.0, true).raise_px(size, top, height, baseline),
            0
        );
        // Moving down is never limited.
        assert_eq!(
            shift(-0.2, 1.8, 2.0, true).raise_px(size, top, height, baseline),
            -2
        );
        // Bottom at -12 px, 4 px under the -8 px floor: 9 px down becomes 5.
        assert_eq!(
            shift(-0.9, -1.2, -1.0, false).raise_px(size, top, height, baseline),
            -5
        );
        // Drawn at -7 px, 3 px down would pass the floor: it stops there.
        assert_eq!(
            shift(-0.3, -1.0, -0.8, false).raise_px(size, top, height, baseline),
            -1
        );
    }

    #[test]
    fn shifts_are_found_by_index_and_ignored_for_another_glyph() {
        let shifts = [
            MarkShift {
                index: 2,
                ..shift(0.1, 0.0, 0.1, true)
            },
            MarkShift {
                index: 5,
                ..shift(0.2, 0.0, 0.1, true)
            },
        ];
        let mut next = 0;
        assert!(shift_for(&shifts, &mut next, 0, 7).is_none());
        assert!(shift_for(&shifts, &mut next, 1, 7).is_none());
        assert_eq!(shift_for(&shifts, &mut next, 2, 7).map(|s| s.y), Some(0.1));
        assert!(shift_for(&shifts, &mut next, 3, 7).is_none());
        assert!(
            shift_for(&shifts, &mut next, 5, 8).is_none(),
            "a reshaped line's other glyph keeps its place"
        );
        assert!(shift_for(&shifts, &mut next, 6, 7).is_none());
    }

    /// Shapes `text` in the bundled JetBrains Mono alone and returns its
    /// glyphs and fallback shifts.
    fn bundled(text: &str) -> (Vec<LayoutGlyph>, Box<[MarkShift]>) {
        use glyphon::cosmic_text::{Attrs, Buffer, Family, Metrics, Shaping, Wrap};
        let mut fonts =
            FontSystem::new_with_locale_and_db("en-US".into(), glyphon::fontdb::Database::new());
        super::super::load_bundled_font(&mut fonts, kettle_config::font::REGULAR);
        let mut swash = SwashCache::new();
        let mut buffer = Buffer::new(&mut fonts, Metrics::new(26.0, 32.0));
        buffer.set_wrap(Wrap::None);
        buffer.set_text(
            text,
            &Attrs::new().family(Family::Name(kettle_config::font::FAMILY)),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut fonts, false);
        let run = buffer.layout_runs().next().expect("one line");
        let glyphs = run.glyphs.to_vec();
        let shifts = line_shifts(&glyphs, run.text, &mut fonts, &mut swash);
        (glyphs, shifts)
    }

    #[test]
    fn marks_the_font_anchors_keep_their_place() {
        // `Z\u{308}` is anchored with a zero offset: only the GPOS anchor
        // tells it from an unplaced mark. The others carry shaped offsets
        // from mark-to-base and mark-to-mark anchors.
        for text in ["Z\u{308}", "q\u{323}", "x\u{308}\u{301}", "s\u{323}\u{307}"] {
            let (glyphs, shifts) = bundled(text);
            assert!(glyphs.len() > 1, "{text:?} must shape to separate marks");
            assert!(glyphs.iter().all(|glyph| glyph.glyph_id != 0), "{text:?}");
            assert!(shifts.is_empty(), "{text:?} moved: {shifts:?}");
        }
    }

    #[test]
    fn marks_without_an_anchor_clear_their_base() {
        let gap = 26.0 * GAP_EM;
        for (text, above) in [("#\u{301}", true), ("1\u{301}", true), ("@\u{323}", false)] {
            let (glyphs, shifts) = bundled(text);
            assert_eq!(glyphs.len(), 2, "{text:?}");
            assert_eq!(shifts.len(), 1, "{text:?}");
            let shift = shifts[0];
            assert_eq!((shift.index, shift.glyph_id), (1, glyphs[1].glyph_id));
            assert_eq!(shift.above, above, "{text:?}");
            let base_ink = {
                let mut fonts = FontSystem::new_with_locale_and_db(
                    "en-US".into(),
                    glyphon::fontdb::Database::new(),
                );
                super::super::load_bundled_font(&mut fonts, kettle_config::font::REGULAR);
                super::ink(&glyphs[0], &mut fonts, &mut SwashCache::new()).unwrap()
            };
            let (top, bottom) = (shift.top * 26.0, shift.bottom * 26.0);
            if above {
                assert!(
                    bottom >= base_ink.top + gap - 0.01,
                    "{text:?} sits on its base"
                );
            } else {
                assert!(
                    top <= base_ink.bottom - gap + 0.01,
                    "{text:?} hangs from its base"
                );
            }
        }
    }
}

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
//! HarfBuzz's fallback does: above classes stack upward from the top of the
//! base's ink, below classes stack downward from its bottom, each with a gap
//! of 1/16 em, centred on (or aligned to) the base's ink. A mark the font
//! attaches, or one the shaper already moved, keeps its position. The grid
//! emit applies the cached shifts through [`MarkShift::moved`] and
//! [`MarkShift::raise_px`], and the block cursor redraws a cell with a moved
//! mark from that emit (`batching::moved_cell_images`).

use std::collections::HashMap;

use glyphon::cosmic_text::{
    CacheKey, CacheKeyFlags, Command, FontFeatures, FontSystem, LayoutGlyph, SubpixelBin,
    SwashCache, fontdb,
};
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

/// One laid-out line's fallback shifts, or `None` while the line has no
/// layout to work them out from (it lies past the buffer's height).
pub(crate) type LineShifts = Option<Box<[MarkShift]>>;

/// The shifts `marks` holds for buffer line `line`, none if not worked out.
pub(crate) fn shifts_of(marks: &[LineShifts], line: usize) -> &[MarkShift] {
    marks
        .get(line)
        .and_then(Option::as_deref)
        .unwrap_or_default()
}

/// The fallback shifts of every line of `buffer`, by line, for the default
/// feature settings.
pub(crate) fn buffer_shifts(
    buffer: &glyphon::Buffer,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
) -> Vec<LineShifts> {
    let mut cache = MarkCache::default();
    buffer
        .lines
        .iter()
        .map(|line| {
            let layout = line.layout_opt()?.first()?;
            Some(line_shifts(
                &layout.glyphs,
                line.text(),
                fonts,
                swash,
                &mut cache,
            ))
        })
        .collect()
}

/// The fallback shifts of one laid-out line of `text`, shaped with the
/// features last given to [`MarkCache::use_features`].
pub(crate) fn line_shifts(
    glyphs: &[LayoutGlyph],
    text: &str,
    fonts: &mut FontSystem,
    swash: &mut SwashCache,
    cache: &mut MarkCache,
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
        place_cluster(
            cluster,
            start,
            cluster_text,
            fonts,
            swash,
            cache,
            &mut shifts,
        );
    }
    shifts.into_boxed_slice()
}

/// Entries each map of a [`MarkCache`] holds before it starts over.
const MARK_CACHE_LIMIT: usize = 4096;

/// The GPOS features harfrust applies unless told otherwise, any of whose
/// lookups may anchor a mark.
const DEFAULT_POSITIONING: [&[u8; 4]; 7] = [
    b"abvm", b"blwm", b"curs", b"dist", b"kern", b"mark", b"mkmk",
];

/// Outline bounds and GPOS answers by glyph. Rows of the same marks ask the
/// same questions, and a screen of tall stacks would otherwise redo them for
/// every reshaped row.
#[derive(Default)]
pub(crate) struct MarkCache {
    /// Outline bounds from the pen, by unhinted raster key.
    bounds: HashMap<CacheKey, Option<Ink>>,
    /// `None` when the font's GPOS could not be read.
    anchors: HashMap<Anchor, Option<bool>>,
    /// The feature settings shaping used, in order, which decide the lookups
    /// that count.
    features: Vec<([u8; 4], u32)>,
}

/// Whether the font anchors `mark` to `to`, a base or an earlier mark.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Anchor {
    font: fontdb::ID,
    to: u16,
    mark: u16,
    to_mark: bool,
}

impl MarkCache {
    pub(crate) fn clear(&mut self) {
        self.bounds.clear();
        self.anchors.clear();
    }

    /// Records the feature settings rows are shaped with, starting over when
    /// they change: a feature turned off, such as `mkmk`, applies no anchor.
    pub(crate) fn use_features(&mut self, features: &FontFeatures) {
        let settings = features
            .features
            .iter()
            .map(|feature| (*feature.tag.as_bytes(), feature.value));
        if !self.features.iter().copied().eq(settings.clone()) {
            self.clear();
            self.features = settings.collect();
        }
    }

    /// Whether shaping applies feature `tag`: on by default for the
    /// [`DEFAULT_POSITIONING`] features, and as the last setting says.
    fn applies(&self, tag: [u8; 4]) -> bool {
        self.features
            .iter()
            .rev()
            .find(|(setting, _)| *setting == tag)
            .map_or(DEFAULT_POSITIONING.contains(&&tag), |&(_, value)| {
                value != 0
            })
    }

    /// The glyph's outline bounds in its line, or `None` for no outline.
    /// Hinting is off so the bounds scale with the font size.
    fn ink(
        &mut self,
        glyph: &LayoutGlyph,
        fonts: &mut FontSystem,
        swash: &mut SwashCache,
    ) -> Option<Ink> {
        let mut key = glyph.physical((0.0, 0.0), 1.0).cache_key;
        key.flags |= CacheKeyFlags::DISABLE_HINTING;
        key.x_bin = SubpixelBin::Zero;
        key.y_bin = SubpixelBin::Zero;
        if self.bounds.len() >= MARK_CACHE_LIMIT && !self.bounds.contains_key(&key) {
            self.bounds.clear();
        }
        let bounds = *self
            .bounds
            .entry(key)
            .or_insert_with(|| outline_bounds(key, fonts, swash));
        let (x, y) = pen(glyph);
        bounds.map(|ink| Ink {
            left: ink.left + x,
            right: ink.right + x,
            bottom: ink.bottom + y,
            top: ink.top + y,
        })
    }

    /// Whether a mark-to-base or mark-to-ligature lookup (`to_mark` false)
    /// or a mark-to-mark lookup (`to_mark` true) that shaping applies anchors
    /// the glyph to glyph `to`. `None` when the font's GPOS cannot be read.
    fn anchored(
        &mut self,
        fonts: &mut FontSystem,
        glyph: &LayoutGlyph,
        to: u16,
        to_mark: bool,
    ) -> Option<bool> {
        let question = Anchor {
            font: glyph.font_id,
            to,
            mark: glyph.glyph_id,
            to_mark,
        };
        if let Some(&answer) = self.anchors.get(&question) {
            return answer;
        }
        if self.anchors.len() >= MARK_CACHE_LIMIT {
            self.anchors.clear();
        }
        let answer = self.ask_gpos(fonts, glyph, question);
        self.anchors.insert(question, answer);
        answer
    }

    /// Answers `question` from the GPOS of `glyph`'s font: `Some(false)` for
    /// a font without GPOS, `None` for one that cannot be read.
    fn ask_gpos(
        &self,
        fonts: &mut FontSystem,
        glyph: &LayoutGlyph,
        question: Anchor,
    ) -> Option<bool> {
        let index = fonts.db().face(glyph.font_id)?.index;
        let font = fonts.get_font(glyph.font_id, glyph.font_weight)?;
        let font = read_fonts::FontRef::from_index(font.data(), index).ok()?;
        let gpos = match font.gpos() {
            Ok(gpos) => gpos,
            Err(read_fonts::ReadError::TableIsMissing(_)) => return Some(false),
            Err(_) => return None,
        };
        let active = self.active_lookups(&gpos).ok()?;
        let (to, mark) = (GlyphId16::new(question.to), GlyphId16::new(question.mark));
        if question.to_mark {
            anchored_to_mark(&gpos, active.as_deref(), to, mark).ok()
        } else {
            anchored_to_base(&gpos, active.as_deref(), to, mark).ok()
        }
    }

    /// Which of the font's lookups shaping can apply: those of the features
    /// it applies, in any script. `None` (all of them) when one of those is
    /// contextual, since the lookups it calls are not followed here.
    fn active_lookups(
        &self,
        gpos: &read_fonts::tables::gpos::Gpos<'_>,
    ) -> Result<Option<Vec<bool>>, read_fonts::ReadError> {
        let lookups = gpos.lookup_list()?;
        let mut active = vec![false; usize::from(lookups.lookup_count())];
        let features = gpos.feature_list()?;
        for record in features.feature_records() {
            if !self.applies(record.feature_tag().to_be_bytes()) {
                continue;
            }
            let feature = record.feature(features.offset_data())?;
            for index in feature.lookup_list_indices() {
                if let Some(slot) = active.get_mut(usize::from(index.get())) {
                    *slot = true;
                }
            }
        }
        for (index, lookup) in lookups.lookups().iter().enumerate() {
            if active.get(index) == Some(&true)
                && matches!(
                    lookup?.subtables()?,
                    PositionSubtables::Contextual(_) | PositionSubtables::ChainContextual(_)
                )
            {
                return Ok(None);
            }
        }
        Ok(Some(active))
    }
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
/// classes HarfBuzz's fallback places. Overlays (1), nukta (7), virama (9),
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
/// This is `position_mark` from HarfBuzz's fallback, aligned on the base's ink
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
    cache: &mut MarkCache,
    shifts: &mut Vec<MarkShift>,
) {
    let Some(classes) = mark_classes(cluster, text, fonts) else {
        return;
    };
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
            base = cache
                .ink(glyph, fonts, swash)
                .map(|ink| (i, ink, pen(glyph).0 + glyph.w));
            if let Some((_, ink, _)) = base {
                stack = ink;
            }
            last_class = None;
            continue;
        }
        let Some((base_index, base_ink, base_end)) = base else {
            continue;
        };
        let Some(class_id) = classes[i] else {
            continue;
        };
        if last_class != Some(class_id) {
            stack = base_ink;
            last_class = Some(class_id);
        }
        let Some(class) = mark_class(class_id) else {
            continue;
        };
        let Some(mark_ink) = cache.ink(glyph, fonts, swash) else {
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
            attachment(cache, fonts, cluster, base_index, i)
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

/// The canonical combining class of each mark glyph in `cluster`, shaped
/// from `text`; `None` for bases and for glyphs whose scalar is unknown.
/// `None` overall when the cluster has no mark.
///
/// The shaper decomposes a precomposed scalar the font lacks, so marks are
/// looked for in the canonical decomposition. A glyph the font's cmap gives
/// for a scalar takes it. GSUB can replace a mark one for one (JetBrains
/// Mono's `.case` forms, for marks it stacks), which keeps the canonical
/// order, so when exactly as many glyphs as scalars are left they pair up in
/// order. Otherwise those glyphs are left as shaped.
fn mark_classes(
    cluster: &[LayoutGlyph],
    text: &str,
    fonts: &mut FontSystem,
) -> Option<Vec<Option<u8>>> {
    let ccc = icu_normalizer::properties::CanonicalCombiningClassMapBorrowed::new();
    let mut decomposed = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd()
        .normalize_iter(text.chars())
        .peekable();
    let starter = decomposed.next_if(|&c| ccc.get_u8(c) == 0);
    let mut scalars: Vec<(char, u8, bool)> = decomposed
        .filter_map(|c| {
            let class = ccc.get_u8(c);
            (class != 0).then_some((c, class, false))
        })
        .collect();
    if scalars.is_empty() {
        return None;
    }
    if let Some(starter) = starter {
        claim_composed(&cluster[0], starter, &mut scalars, fonts);
    }
    let mut classes = vec![None; cluster.len()];
    let mut unmatched = Vec::new();
    for (i, glyph) in cluster.iter().enumerate() {
        if i == 0 || glyph.w != 0.0 {
            continue;
        }
        let nominal = fonts
            .get_font(glyph.font_id, glyph.font_weight)
            .and_then(|font| {
                let charmap = font.as_swash().charmap();
                scalars
                    .iter()
                    .position(|&(c, _, claimed)| !claimed && charmap.map(c) == glyph.glyph_id)
            });
        match nominal {
            Some(slot) => {
                scalars[slot].2 = true;
                classes[i] = Some(scalars[slot].1);
            }
            None => unmatched.push(i),
        }
    }
    let left: Vec<u8> = scalars
        .iter()
        .filter(|&&(_, _, claimed)| !claimed)
        .map(|&(_, class, _)| class)
        .collect();
    if left.len() == unmatched.len() {
        for (i, class) in unmatched.into_iter().zip(left) {
            classes[i] = Some(class);
        }
    }
    Some(classes)
}

/// Claims the marks the shaper composed into `base`, the cluster's first
/// glyph, so they are not paired with a substituted mark. harfrust composes
/// a starter with each following mark, in canonical order and unless an
/// earlier mark it kept blocks it, when the font has the composite:
/// `A\u{301}\u{307}` is drawn as `Aacute` and a dot.
fn claim_composed(
    base: &LayoutGlyph,
    starter: char,
    scalars: &mut [(char, u8, bool)],
    fonts: &mut FontSystem,
) {
    let Some(font) = fonts.get_font(base.font_id, base.font_weight) else {
        return;
    };
    let charmap = font.as_swash().charmap();
    let compose = icu_normalizer::properties::CanonicalCompositionBorrowed::new();
    let mut composite = starter;
    let mut kept = 0;
    let mut absorbed = Vec::new();
    for (slot, &(mark, class, _)) in scalars.iter().enumerate() {
        match compose.compose(composite, mark) {
            Some(next) if kept < class && charmap.map(next) != 0 => {
                composite = next;
                absorbed.push(slot);
            }
            _ => kept = class,
        }
    }
    if !absorbed.is_empty() && charmap.map(composite) == base.glyph_id {
        for slot in absorbed {
            scalars[slot].2 = true;
        }
    }
}

/// How the font anchors the mark at `index` in `cluster`, whose base is at
/// `base`. Mark-to-mark lookups run after mark-to-base ones, so an anchor to
/// the nearest earlier mark wins. When in doubt the mark keeps its place.
fn attachment(
    cache: &mut MarkCache,
    fonts: &mut FontSystem,
    cluster: &[LayoutGlyph],
    base: usize,
    index: usize,
) -> Attachment {
    let glyph = &cluster[index];
    for earlier in (base + 1..index).rev() {
        if cluster[earlier].font_id != glyph.font_id {
            continue;
        }
        match cache.anchored(fonts, glyph, cluster[earlier].glyph_id, true) {
            Some(true) => return Attachment::Mark(earlier),
            Some(false) => {}
            None => return Attachment::Base,
        }
    }
    if cluster[base].font_id != glyph.font_id {
        return Attachment::None;
    }
    match cache.anchored(fonts, glyph, cluster[base].glyph_id, false) {
        Some(false) => Attachment::None,
        Some(true) | None => Attachment::Base,
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

/// The outline bounds of the glyph `key` names, from its pen, or `None` for
/// no outline.
fn outline_bounds(key: CacheKey, fonts: &mut FontSystem, swash: &mut SwashCache) -> Option<Ink> {
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
    bounds.filter(|ink| ink.right > ink.left && ink.top > ink.bottom)
}

type Anchors =
    [read_fonts::types::BigEndian<read_fonts::types::Nullable<read_fonts::types::Offset16>>];

/// Whether `anchors`, one per mark class, has one for `class`.
fn has_anchor(anchors: &Anchors, class: u16) -> bool {
    anchors
        .get(usize::from(class))
        .is_some_and(|anchor| !anchor.get().is_null())
}

/// The lookups of `gpos` that `active` (all when `None`) lets apply.
fn applied_lookups<'a>(
    gpos: &read_fonts::tables::gpos::Gpos<'a>,
    active: Option<&[bool]>,
) -> Result<
    impl Iterator<Item = Result<read_fonts::tables::gpos::PositionLookup<'a>, read_fonts::ReadError>>,
    read_fonts::ReadError,
> {
    Ok(gpos
        .lookup_list()?
        .lookups()
        .iter()
        .enumerate()
        .filter(move |(index, _)| active.is_none_or(|active| active.get(*index) == Some(&true)))
        .map(|(_, lookup)| lookup))
}

/// Whether an applied mark-to-base or mark-to-ligature lookup anchors `mark`
/// to `base`.
fn anchored_to_base(
    gpos: &read_fonts::tables::gpos::Gpos<'_>,
    active: Option<&[bool]>,
    base: GlyphId16,
    mark: GlyphId16,
) -> Result<bool, read_fonts::ReadError> {
    for lookup in applied_lookups(gpos, active)? {
        match lookup?.subtables()? {
            PositionSubtables::MarkToBase(subtables) => {
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
                    if has_anchor(record.base_anchor_offsets(), class) {
                        return Ok(true);
                    }
                }
            }
            PositionSubtables::MarkToLig(subtables) => {
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
                        if has_anchor(component?.ligature_anchor_offsets(), class) {
                            return Ok(true);
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Ok(false)
}

/// Whether an applied mark-to-mark lookup anchors `mark` to the earlier
/// `previous`.
fn anchored_to_mark(
    gpos: &read_fonts::tables::gpos::Gpos<'_>,
    active: Option<&[bool]>,
    previous: GlyphId16,
    mark: GlyphId16,
) -> Result<bool, read_fonts::ReadError> {
    for lookup in applied_lookups(gpos, active)? {
        let PositionSubtables::MarkToMark(subtables) = lookup?.subtables()? else {
            continue;
        };
        for subtable in subtables.iter() {
            let subtable = subtable?;
            let (Some(mark_index), Some(previous_index)) = (
                subtable.mark1_coverage()?.get(mark),
                subtable.mark2_coverage()?.get(previous),
            ) else {
                continue;
            };
            let class = mark_class_of(&subtable.mark1_array()?, mark_index)?;
            let record = subtable
                .mark2_array()?
                .mark2_records()
                .get(usize::from(previous_index))?;
            if has_anchor(record.mark2_anchor_offsets(), class) {
                return Ok(true);
            }
        }
    }
    Ok(false)
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

    /// Shapes `text` in the bundled JetBrains Mono alone, at 26 px.
    fn shape_bundled(text: &str) -> (FontSystem, Vec<LayoutGlyph>) {
        shape_bundled_with(text, &FontFeatures::new())
    }

    fn shape_bundled_with(text: &str, features: &FontFeatures) -> (FontSystem, Vec<LayoutGlyph>) {
        use glyphon::cosmic_text::{Attrs, Buffer, Family, Metrics, Shaping, Wrap};
        let mut fonts =
            FontSystem::new_with_locale_and_db("en-US".into(), glyphon::fontdb::Database::new());
        super::super::load_bundled_font(&mut fonts, kettle_config::font::REGULAR);
        let mut buffer = Buffer::new(&mut fonts, Metrics::new(26.0, 32.0));
        buffer.set_wrap(Wrap::None);
        buffer.set_text(
            text,
            &Attrs::new()
                .family(Family::Name(kettle_config::font::FAMILY))
                .font_features(features.clone()),
            Shaping::Advanced,
            None,
        );
        buffer.shape_until_scroll(&mut fonts, false);
        let glyphs = buffer
            .layout_runs()
            .next()
            .expect("one line")
            .glyphs
            .to_vec();
        (fonts, glyphs)
    }

    /// The glyphs and fallback shifts of `text` in the bundled font alone.
    fn bundled(text: &str) -> (Vec<LayoutGlyph>, Box<[MarkShift]>) {
        let (mut fonts, glyphs) = shape_bundled(text);
        let shifts = line_shifts(
            &glyphs,
            text,
            &mut fonts,
            &mut SwashCache::new(),
            &mut MarkCache::default(),
        );
        (glyphs, shifts)
    }

    #[test]
    fn the_mark_cache_answers_repeats_and_stays_bounded() {
        let text = "#\u{301}#\u{301}#\u{301}";
        let (mut fonts, glyphs) = shape_bundled(text);
        let (mut swash, mut cache) = (SwashCache::new(), MarkCache::default());
        let shifts = line_shifts(&glyphs, text, &mut fonts, &mut swash, &mut cache);
        assert_eq!(shifts.len(), 3, "each accent is placed");
        assert_eq!(
            (cache.bounds.len(), cache.anchors.len()),
            (2, 1),
            "one bounds per glyph and one GPOS answer, however often they repeat"
        );

        cache.clear();
        let mut key = glyphs[0].physical((0.0, 0.0), 1.0).cache_key;
        for glyph_id in 0..MARK_CACHE_LIMIT as u16 {
            key.glyph_id = glyph_id + 1000;
            cache.bounds.insert(key, None);
        }
        assert!(cache.ink(&glyphs[0], &mut fonts, &mut swash).is_some());
        assert_eq!(cache.bounds.len(), 1, "a full cache starts over");
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
            let (mut fonts, glyphs) = shape_bundled(text);
            let (mut swash, mut cache) = (SwashCache::new(), MarkCache::default());
            let shifts = line_shifts(&glyphs, text, &mut fonts, &mut swash, &mut cache);
            assert_eq!(glyphs.len(), 2, "{text:?}");
            assert_eq!(shifts.len(), 1, "{text:?}");
            let shift = shifts[0];
            assert_eq!((shift.index, shift.glyph_id), (1, glyphs[1].glyph_id));
            assert_eq!(shift.above, above, "{text:?}");
            let base_ink = cache.ink(&glyphs[0], &mut fonts, &mut swash).unwrap();
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

    /// JetBrains Mono swaps stacked marks for its `.case` forms, which no
    /// cmap entry names. They keep their scalars' order, so they are placed.
    #[test]
    fn marks_the_font_substitutes_are_placed_by_their_scalars() {
        let text = "@\u{323}\u{307}";
        let (mut fonts, glyphs) = shape_bundled(text);
        let (mut swash, mut cache) = (SwashCache::new(), MarkCache::default());
        let shifts = line_shifts(&glyphs, text, &mut fonts, &mut swash, &mut cache);
        let base = cache.ink(&glyphs[0], &mut fonts, &mut swash).unwrap();
        let [below, above] = &shifts[..] else {
            panic!("both marks are placed: {shifts:?}");
        };
        assert!(!below.above && below.top * 26.0 <= base.bottom);
        assert!(above.above && above.bottom * 26.0 >= base.top);
    }

    /// `A\u{301}\u{307}` is drawn as `Aacute` and a substituted dot: the
    /// acute composed into the base is not left over to block the pairing.
    #[test]
    fn a_substituted_mark_after_a_composed_base_is_placed() {
        use glyphon::cosmic_text::FeatureTag;
        let mut features = FontFeatures::new();
        features.disable(FeatureTag::new(b"mark"));
        let text = "A\u{301}\u{307}";
        let (mut fonts, glyphs) = shape_bundled_with(text, &features);
        assert_eq!(
            glyphs.len(),
            2,
            "the fixture needs a composed base and a mark"
        );
        let (mut swash, mut cache) = (SwashCache::new(), MarkCache::default());
        cache.use_features(&features);
        let shifts = line_shifts(&glyphs, text, &mut fonts, &mut swash, &mut cache);
        let base = cache.ink(&glyphs[0], &mut fonts, &mut swash).unwrap();
        let [dot] = &shifts[..] else {
            panic!("the dot is placed: {shifts:?}");
        };
        assert!(dot.above && dot.bottom * 26.0 >= base.top);
    }

    /// With `mkmk` turned off the font's mark-to-mark anchors apply to
    /// nothing: the acute stacks on the diaeresis instead of keeping the
    /// diaeresis's place.
    #[test]
    fn a_feature_turned_off_anchors_nothing() {
        use glyphon::cosmic_text::FeatureTag;
        let mut features = FontFeatures::new();
        features
            .disable(FeatureTag::new(b"ccmp"))
            .disable(FeatureTag::new(b"mkmk"));
        let text = "#\u{308}\u{301}";
        let (mut fonts, glyphs) = shape_bundled_with(text, &features);
        assert!(
            glyphs[1..]
                .iter()
                .all(|glyph| glyph.x_offset == 0.0 && glyph.y_offset == 0.0),
            "the fixture needs marks the shaper left alone"
        );
        let (mut swash, mut cache) = (SwashCache::new(), MarkCache::default());
        cache.use_features(&features);
        let shifts = line_shifts(&glyphs, text, &mut fonts, &mut swash, &mut cache);
        let [diaeresis, acute] = &shifts[..] else {
            panic!("both marks are placed: {shifts:?}");
        };
        assert!(
            acute.bottom >= diaeresis.top,
            "the acute stacks above the diaeresis: {acute:?} over {diaeresis:?}"
        );
    }
}

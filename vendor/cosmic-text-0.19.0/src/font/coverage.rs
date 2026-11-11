// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which fallback faces could give a glyph to a cluster that is still missing.
//!
//! Kettle-owned. A run whose cluster no face covers used to be shaped with
//! every face in the database, building a shape plan for each one, every time
//! the run was shaped. `FontFallbackIter` now asks these questions first and
//! skips a face only when its character map proves that shaping with it could
//! not fill any missing cluster. Whenever that cannot be shown, the face is
//! tried as upstream tries it.

use alloc::vec::Vec;
use skrifa::raw::tables::cmap::{Cmap, CmapSubtable, PlatformId};
use skrifa::raw::TableProvider as _;

/// Clusters no face maps, remembered at most this many at once. The set is
/// cleared when it is full: a screen with more distinct such clusters than
/// this walks the faces' character maps again for each one, at the cost of a
/// character-map read per face rather than a shaping per face.
pub(crate) const MISSING_CLUSTERS_LIMIT: usize = 4096;

/// Longer clusters are never remembered: a terminal cell can carry a long run
/// of combining marks, and the set must stay small.
pub(crate) const MISSING_CLUSTER_MAX_BYTES: usize = 64;

/// Whether HarfBuzz can give `c` a glyph without the face's character map
/// holding `c` or a canonical equivalent of it. A cluster holding one is
/// never skipped past and never remembered as missing.
///
/// - Default-ignorable code points are drawn with the space glyph or an
///   invisible one (`hide_default_ignorables`); this also covers the
///   variation selectors, which use the face's format 14 subtable.
/// - `shape_fallback` shapes a tab as a space.
/// - The spaces of `space_fallback` and U+2011 borrow U+0020 and U+2010
///   (`decompose_current_character`).
/// - The Hebrew shaper composes to presentation forms, and the Thai shaper
///   maps marks to private-use glyphs, by the face's own character map.
/// - harfrust 0.5 sends Lao through the Thai shaper (`ot_shaper.rs` 156-157),
///   whose `preprocess_text` splits SARA AM, U+0E33 and U+0EB3 alike
///   (`is_sara_am` masks with `!0x0080`, `ot_shaper_thai.rs` 358-368), into
///   NIKHAHIT and SARA AA. Lao's SARA AM decomposes only by compatibility, so
///   neither normalization below finds those parts.
pub(crate) fn shaper_substitutes(c: char) -> bool {
    matches!(
        u32::from(c),
        // Default_Ignorable_Code_Point, DerivedCoreProperties.txt
        0x00AD
            | 0x034F
            | 0x061C
            | 0x115F..=0x1160
            | 0x17B4..=0x17B5
            | 0x180B..=0x180F
            | 0x200B..=0x200F
            | 0x202A..=0x202E
            | 0x2060..=0x206F
            | 0x3164
            | 0xFE00..=0xFE0F
            | 0xFEFF
            | 0xFFA0
            | 0xFFF0..=0xFFF8
            | 0x1BCA0..=0x1BCA3
            | 0x1D173..=0x1D17A
            | 0xE0000..=0xE0FFF
            // `shape_fallback` shapes a tab as U+0020
            | 0x0009
            // harfrust `space_fallback`, and U+2011 NON-BREAKING HYPHEN
            | 0x0020
            | 0x00A0
            | 0x2000..=0x200A
            | 0x2011
            | 0x202F
            | 0x205F
            | 0x3000
            // Hebrew, Thai and Lao blocks
            | 0x0590..=0x05FF
            | 0x0E00..=0x0E7F
            | 0x0E80..=0x0EFF
    )
}

/// The characters a face must map at least one of to give `cluster` a glyph:
/// its own characters and those of its canonical decomposition and
/// composition, which HarfBuzz uses when the face lacks the character as
/// written. `None` when the cluster holds a character the shaper can draw some
/// other way, and no face may be skipped for it.
///
/// A right-to-left run is never given a set: harfrust draws a mirrored
/// character with its Bidi_Mirroring_Glyph when the face has that
/// (`rotate_chars`, `ot_shape.rs` 777-791), and that is not looked up here.
pub fn required_chars(cluster: &str, rtl: bool) -> Option<Vec<char>> {
    if rtl || cluster.chars().any(shaper_substitutes) {
        return None;
    }
    let nfd = icu_normalizer::DecomposingNormalizerBorrowed::new_nfd();
    let nfc = icu_normalizer::ComposingNormalizerBorrowed::new_nfc();
    let mut chars = Vec::new();
    let forms = cluster
        .chars()
        .chain(nfd.normalize_iter(cluster.chars()))
        .chain(nfc.normalize_iter(cluster.chars()));
    for c in forms {
        if shaper_substitutes(c) {
            return None;
        }
        if !chars.contains(&c) {
            chars.push(c);
        }
    }
    Some(chars)
}

/// Whether the face in `data` maps any of `chars` to a glyph other than
/// `.notdef`, in the subtable harfrust shapes with (`find_best_cmap_subtable`
/// in its `tables.rs`), including its symbol-font remapping. Only `false` lets
/// a face be skipped, so every case this cannot settle answers `true`: a face
/// that does not parse, has no readable character map or no subtable harfrust
/// would use, or uses a Mac Roman subtable, which harfrust remaps.
pub fn face_maps_any(data: &[u8], index: u32, chars: &[char]) -> bool {
    with_cmap(data, index, |maps| {
        maps.is_none_or(|maps| chars.iter().any(|&c| maps(c)))
    })
}

/// Calls `f` with a lookup of the face's nominal glyphs, or with `None` when
/// the face's character map cannot be read the way harfrust reads it.
pub(crate) fn with_cmap<T>(
    data: &[u8],
    index: u32,
    f: impl FnOnce(Option<&dyn Fn(char) -> bool>) -> T,
) -> T {
    let Ok(font) = skrifa::FontRef::from_index(data, index) else {
        return f(None);
    };
    let Ok(cmap) = font.cmap() else {
        return f(None);
    };
    let Some((subtable, platform, encoding)) = best_subtable(&cmap) else {
        return f(None);
    };
    if platform == PlatformId::Macintosh {
        return f(None);
    }
    let is_symbol = platform == PlatformId::Windows && encoding == WINDOWS_SYMBOL;
    let maps = |c: char| {
        let nominal = |c: u32| subtable.map_codepoint(c).is_some_and(|g| g.to_u32() != 0);
        let c = u32::from(c);
        nominal(c) || (is_symbol && c <= 0x00FF && nominal(0xF000 + c))
    };
    f(Some(&maps))
}

const WINDOWS_SYMBOL: u16 = 0;

/// harfrust 0.5's `find_best_cmap_subtable` (`tables.rs` 209-255): the
/// platform and encoding pairs in its order, and within a pair the first
/// record whose subtable parses as a format it maps with, passing over records
/// that do not, as its `find_cmap_subtable` does.
fn best_subtable<'a>(cmap: &Cmap<'a>) -> Option<(CmapSubtable<'a>, PlatformId, u16)> {
    const ORDER: [(PlatformId, u16); 10] = [
        (PlatformId::Windows, WINDOWS_SYMBOL),
        (PlatformId::Windows, 10),
        (PlatformId::Unicode, 6),
        (PlatformId::Unicode, 4),
        (PlatformId::Windows, 1),
        (PlatformId::Unicode, 3),
        (PlatformId::Unicode, 2),
        (PlatformId::Unicode, 1),
        (PlatformId::Unicode, 0),
        (PlatformId::Macintosh, 0),
    ];
    let data = cmap.offset_data();
    ORDER.iter().find_map(|&(platform, encoding)| {
        cmap.encoding_records().iter().find_map(|record| {
            if record.platform_id() != platform || record.encoding_id() != encoding {
                return None;
            }
            match record.subtable(data).ok()? {
                subtable @ (CmapSubtable::Format0(_)
                | CmapSubtable::Format4(_)
                | CmapSubtable::Format6(_)
                | CmapSubtable::Format10(_)
                | CmapSubtable::Format12(_)
                | CmapSubtable::Format13(_)) => Some((subtable, platform, encoding)),
                _ => None,
            }
        })
    })
}

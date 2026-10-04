//! Per-job outline fonts: the bundled face and explicitly supplied files.
//! No host discovery or library file loader is enabled. Untrusted collection
//! bytes are parsed for one selected face, never by an all-face loader.

use std::collections::{BTreeSet, HashSet};
use std::os::unix::ffi::OsStrExt as _;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use kettle_media::{
    FailureCode, FallbackFont, MAX_FALLBACK_FONT_BYTES, MAX_FALLBACK_FONT_TOTAL_BYTES,
    MAX_FALLBACK_FONTS, MAX_SVG_WORK, MAX_UNCOVERED_SCRIPTS, Warning,
};
use skrifa::raw::{TableProvider as _, types::Tag};
use skrifa::string::StringId;
use skrifa::{FontRef, MetadataProvider as _};
use unicode_script::UnicodeScript as _;
use usvg::fontdb;

const MAX_TABLES: usize = 128;
const MAX_NAME_TABLE_BYTES: usize = 64 * 1024;
const MAX_NAME_RECORDS: usize = 512;
const MAX_NAME_BYTES: usize = 4096;
const MAX_AXES: usize = 64;
const MAX_CMAP_RECORDS: usize = 64;

/// JetBrains Mono (the Nerd Font build Kettle already ships), SIL Open Font
/// License 1.1; see NOTICE.
static BUNDLED: &[u8] =
    include_bytes!("../../../../assets/fonts/JetBrainsMonoNerdFont-Regular.ttf");

/// The bundled face's database and family name, built once.
pub(crate) fn bundled() -> &'static (Arc<fontdb::Database>, String) {
    static FONTS: OnceLock<(Arc<fontdb::Database>, String)> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut database = fontdb::Database::new();
        database.load_font_source(fontdb::Source::Binary(Arc::new(BUNDLED)));
        let family = database
            .faces()
            .next()
            .and_then(|face| face.families.first())
            .map(|(name, _)| name.clone())
            .unwrap_or_default();
        database.set_serif_family(family.clone());
        database.set_sans_serif_family(family.clone());
        database.set_monospace_family(family.clone());
        database.set_cursive_family(family.clone());
        database.set_fantasy_family(family.clone());
        (Arc::new(database), family)
    })
}

pub(crate) struct JobFonts {
    pub database: Arc<fontdb::Database>,
    pub family: String,
    bundled_id: fontdb::ID,
}

pub(crate) fn for_job(entries: &[FallbackFont]) -> Result<JobFonts, FailureCode> {
    if entries.len() > MAX_FALLBACK_FONTS {
        return Err(FailureCode::BadParams);
    }
    let (bundled, family) = bundled();
    let mut database = bundled.clone();
    let bundled_id = bundled.faces().next().ok_or(FailureCode::RenderParse)?.id;
    let mut total = 0;
    for entry in entries {
        let cap = MAX_FALLBACK_FONT_BYTES.min(MAX_FALLBACK_FONT_TOTAL_BYTES - total);
        let snapshot = crate::source::load_regular_path(
            Path::new(std::ffi::OsStr::from_bytes(entry.path.as_bytes())),
            None,
            cap,
            || {},
        )?;
        let bytes = Arc::new(snapshot.bytes.into_owned());
        total += bytes.len();
        let face = crate::guarded(FailureCode::RenderParse, || {
            selected_face(bytes.clone(), entry.face_index)
        })?;
        Arc::make_mut(&mut database).push_face_info(face);
    }
    Ok(JobFonts {
        database,
        family: family.clone(),
        bundled_id,
    })
}

fn name(
    font: &FontRef<'_>,
    id: StringId,
) -> Result<Option<(String, fontdb::Language)>, FailureCode> {
    let Some(value) = font.localized_strings(id).english_or_first() else {
        return Ok(None);
    };
    let language = match value.language() {
        Some("en" | "en-US") => fontdb::Language::English_UnitedStates,
        _ => fontdb::Language::Unknown,
    };
    let mut text = String::new();
    for c in value.chars() {
        if text.len() + c.len_utf8() > MAX_NAME_BYTES {
            return Err(FailureCode::RenderResource);
        }
        text.push(c);
    }
    if text.is_empty() || text.chars().any(char::is_control) {
        return Err(FailureCode::RenderParse);
    }
    Ok(Some((text, language)))
}

fn selected_face(bytes: Arc<Vec<u8>>, index: u32) -> Result<fontdb::FaceInfo, FailureCode> {
    let font = FontRef::from_index(&bytes, index).map_err(|_| FailureCode::RenderParse)?;
    let records = font.table_directory().table_records();
    if records.len() > MAX_TABLES {
        return Err(FailureCode::RenderResource);
    }
    let mut previous = None;
    for record in records {
        let tag = record.tag();
        // Sorted unique tags keep all parsers on the same table. Reject tag
        // presence, even if malformed data would make table_data return None.
        if previous.is_some_and(|p| p >= tag) {
            return Err(FailureCode::RenderParse);
        }
        previous = Some(tag);
        if [
            b"SVG ", b"COLR", b"CPAL", b"CBDT", b"CBLC", b"sbix", b"EBDT", b"EBLC", b"EBSC",
            b"bdat", b"bloc",
        ]
        .iter()
        .any(|blocked| tag == Tag::new(blocked))
        {
            // usvg parses SVG/color glyphs through a separate default-options
            // path. Only outline glyphs stay inside the document's resolvers.
            return Err(FailureCode::RenderParse);
        }
    }
    let naming = font
        .table_data(Tag::new(b"name"))
        .ok_or(FailureCode::RenderParse)?;
    if naming.len() > MAX_NAME_TABLE_BYTES
        || font
            .name()
            .map_err(|_| FailureCode::RenderParse)?
            .name_record()
            .len()
            > MAX_NAME_RECORDS
        || font.axes().len() > MAX_AXES
    {
        return Err(FailureCode::RenderResource);
    }
    let cmap = font.cmap().map_err(|_| FailureCode::RenderParse)?;
    if cmap.encoding_records().len() > MAX_CMAP_RECORDS {
        return Err(FailureCode::RenderResource);
    }
    let units = font
        .head()
        .map_err(|_| FailureCode::RenderParse)?
        .units_per_em();
    if !(16..=16384).contains(&units)
        || font
            .maxp()
            .map_err(|_| FailureCode::RenderParse)?
            .num_glyphs()
            == 0
        || [b"glyf", b"CFF ", b"CFF2"]
            .iter()
            .all(|tag| font.table_data(Tag::new(tag)).is_none())
    {
        return Err(FailureCode::RenderParse);
    }
    font.hmtx().map_err(|_| FailureCode::RenderParse)?;
    let family = match name(&font, StringId::TYPOGRAPHIC_FAMILY_NAME)? {
        Some(value) => value,
        None => name(&font, StringId::FAMILY_NAME)?.ok_or(FailureCode::RenderParse)?,
    };
    let post_script_name = name(&font, StringId::POSTSCRIPT_NAME)?
        .map(|value| value.0)
        .unwrap_or_else(|| family.0.clone());
    let attributes = font.attributes();
    let style = match attributes.style {
        skrifa::attribute::Style::Normal => fontdb::Style::Normal,
        skrifa::attribute::Style::Italic => fontdb::Style::Italic,
        skrifa::attribute::Style::Oblique(_) => fontdb::Style::Oblique,
    };
    let stretch = match font.os2().map(|os2| os2.us_width_class()).unwrap_or(5) {
        1 => fontdb::Stretch::UltraCondensed,
        2 => fontdb::Stretch::ExtraCondensed,
        3 => fontdb::Stretch::Condensed,
        4 => fontdb::Stretch::SemiCondensed,
        6 => fontdb::Stretch::SemiExpanded,
        7 => fontdb::Stretch::Expanded,
        8 => fontdb::Stretch::ExtraExpanded,
        9 => fontdb::Stretch::UltraExpanded,
        _ => fontdb::Stretch::Normal,
    };
    let monospaced = font.post().is_ok_and(|post| post.is_fixed_pitch() != 0);
    Ok(fontdb::FaceInfo {
        id: fontdb::ID::dummy(),
        source: fontdb::Source::Binary(bytes),
        index,
        families: vec![family],
        post_script_name,
        style,
        weight: fontdb::Weight(attributes.weight.value().clamp(1.0, 1000.0) as u16),
        stretch,
        monospaced,
    })
}

pub(crate) fn resolver() -> usvg::FontResolver<'static> {
    usvg::FontResolver {
        select_font: usvg::FontResolver::default_font_selector(),
        select_fallback: Box::new(|c, excluded, database| {
            let base = excluded.first().and_then(|id| database.face(*id));
            database
                .faces()
                .filter(|face| !excluded.contains(&face.id))
                .min_by_key(|face| {
                    let covers = database.with_face_data(face.id, |bytes, index| {
                        FontRef::from_index(bytes, index).ok().is_some_and(|font| {
                            font.charmap()
                                .map(c)
                                .is_some_and(|glyph| glyph.to_u32() != 0)
                        })
                    }) == Some(true);
                    let distance = base
                        .map(|base| {
                            (
                                u8::from(base.style != face.style),
                                base.stretch.to_number().abs_diff(face.stretch.to_number()),
                                base.weight.0.abs_diff(face.weight.0),
                            )
                        })
                        .unwrap_or_default();
                    // usvg reshapes the whole run, but asks only about its
                    // first missing character. Try other supplied faces even
                    // when that character is uncovered, so later glyphs can
                    // resolve. Its exclusion list bounds this to nine faces.
                    (u8::from(!covers), distance)
                })
                .map(|face| face.id)
        }),
    }
}

pub(crate) struct Coverage {
    pub scripts: Vec<String>,
    pub warnings: Vec<Warning>,
}

impl JobFonts {
    /// Inspect shaped glyphs, rather than fallback callbacks: usvg can stop
    /// trying fallbacks at the first missing character in a text run.
    pub fn coverage(&self, tree: &usvg::Tree) -> Result<Coverage, FailureCode> {
        let mut groups = vec![tree.root()];
        groups.extend(tree.patterns().iter().map(|pattern| pattern.root()));
        groups.extend(tree.clip_paths().iter().map(|clip| clip.root()));
        groups.extend(tree.masks().iter().map(|mask| mask.root()));
        for filter in tree.filters() {
            for primitive in filter.primitives() {
                if let usvg::filter::Kind::Image(image) = primitive.kind() {
                    groups.push(image.root());
                }
            }
        }
        let mut seen = HashSet::new();
        let mut steps = 0_u64;
        let mut scripts = BTreeSet::new();
        let mut fallback = false;
        let mut missing = false;
        while let Some(group) = groups.pop() {
            if !seen.insert(std::ptr::from_ref(group)) {
                continue;
            }
            for node in group.children() {
                steps += 1;
                if steps > MAX_SVG_WORK {
                    return Err(FailureCode::RenderResource);
                }
                match node {
                    usvg::Node::Group(group) => groups.push(group),
                    usvg::Node::Text(text) => {
                        for span in text.layouted() {
                            if !span.visible || (span.fill.is_none() && span.stroke.is_none()) {
                                continue;
                            }
                            for glyph in &span.positioned_glyphs {
                                steps += 1;
                                if steps > MAX_SVG_WORK {
                                    return Err(FailureCode::RenderResource);
                                }
                                if glyph.id.0 != 0 {
                                    fallback |= glyph.font != self.bundled_id;
                                    continue;
                                }
                                for c in glyph.text.chars() {
                                    steps += 1;
                                    if steps > MAX_SVG_WORK {
                                        return Err(FailureCode::RenderResource);
                                    }
                                    if c.is_control() || c.is_whitespace() {
                                        continue;
                                    }
                                    missing = true;
                                    scripts.insert(c.script().full_name());
                                    if scripts.len() > MAX_UNCOVERED_SCRIPTS {
                                        return Err(FailureCode::RenderResource);
                                    }
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        let mut warnings = Vec::new();
        if missing {
            warnings.push(Warning::MissingGlyphs);
        }
        if fallback {
            warnings.push(Warning::FontFallback);
        }
        Ok(Coverage {
            scripts: scripts.into_iter().map(str::to_owned).collect(),
            warnings,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_bundled_face_is_loaded() {
        let (database, family) = bundled();
        assert_eq!(database.len(), 1);
        assert!(!family.is_empty());
        // Any family, generic or named, resolves to that face.
        for families in [
            &[fontdb::Family::Name("Arial")][..],
            &[fontdb::Family::SansSerif],
            &[fontdb::Family::Serif],
            &[fontdb::Family::Monospace],
        ] {
            let query = fontdb::Query {
                families,
                ..fontdb::Query::default()
            };
            let fallback = [families, &[fontdb::Family::Serif]].concat();
            let id = database
                .query(&query)
                .or_else(|| {
                    database.query(&fontdb::Query {
                        families: &fallback,
                        ..fontdb::Query::default()
                    })
                })
                .unwrap();
            assert_eq!(Some(id), database.faces().next().map(|face| face.id));
        }
    }

    #[test]
    fn fallback_without_a_base_face_is_bounded_and_does_not_panic() {
        let mut database = bundled().0.clone();
        let id = (resolver().select_fallback)('A', &[], &mut database).unwrap();
        assert_eq!(id, database.faces().next().unwrap().id);
        assert!((resolver().select_fallback)('A', &[id], &mut database).is_none());
        assert_eq!(database.len(), 1);
    }
}

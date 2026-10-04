//! The fonts SVG text may use: the face bundled here, and nothing from the
//! host. The database is built from bytes in this binary, with no file read
//! and no system font discovery (usvg's font crate is built without either),
//! and every generic family names the bundled face, so any `font-family` an
//! SVG asks for resolves to it.

use std::sync::{Arc, OnceLock};

use usvg::fontdb;

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
}

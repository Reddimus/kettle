//! A pane's media shelf: the last few items shown in it, newest first.
//!
//! The shelf holds at most [`MAX_SHELF_ITEMS`]. A new item with the key of
//! one already there replaces it in place, keeping its id; otherwise it goes
//! first, and a full shelf drops its least recently viewed item, never the
//! one on screen. Pixels are charged to the process preview account, and an
//! item whose pixels had to go to make room keeps its place and details.

use kettle_core::ImageData;
use kettle_media::{MediaKind, Warning};

/// Items one pane's shelf keeps.
pub(crate) const MAX_SHELF_ITEMS: usize = 8;

/// Who sent an item, as far as Kettle could check.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Provenance {
    /// A process running in this pane, by its ancestry.
    Verified,
    /// A sender Kettle could not place in this pane: routed by full
    /// control's choice of pane or by the sender's own environment.
    Unverified(UnverifiedSender),
    /// The user, previewing a file the pane's output names.
    User,
}

/// What Kettle read about an unverified sender from the operating system,
/// never from the sender's own words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UnverifiedSender {
    /// The executable the kernel names for the program that asked, sanitized
    /// for display; `None` when it could not be read.
    pub executable: Option<String>,
    pub pid: u32,
    /// Who signed that program's code, sanitized for display; `None` when
    /// it is not validly signed with a certificate Apple issued, or the
    /// platform has no code signatures.
    pub signer: Option<String>,
}

/// How a signer reads: Apple for Apple's own code, else its certificate's
/// name without the certificate's kind (`Developer ID Application: `), else
/// its team, else its signing identifier.
pub(crate) fn signer_name(signature: &kettle_ctl::signing::Signature) -> String {
    if signature.apple {
        return "Apple".to_owned();
    }
    match (signature.authority.as_deref(), &signature.team) {
        (Some(authority), _) => authority
            .split_once(": ")
            .map_or(authority, |(_, name)| name)
            .to_owned(),
        (None, Some(team)) => team.clone(),
        (None, None) => signature.identifier.clone(),
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ItemPixels {
    Ready(ImageData),
    /// Released so newer previews fit in the preview account.
    Evicted,
}

/// What replaces an item in place when it is published again.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ShelfKey {
    /// The key a `show` request named.
    Explicit(String),
    /// A file, by its native path: two paths that print alike stay apart.
    Path(kettle_media::NativePath),
}

#[derive(Clone, Debug)]
pub(crate) struct ShelfItem {
    /// Process-unique and stable while the item stays on a shelf, through
    /// replacements by key.
    pub id: u64,
    /// Bumped by every replacement, so a stale view can tell.
    pub generation: u64,
    pub key: Option<ShelfKey>,
    /// Display-ready: sanitized and bounded.
    pub title: String,
    pub provenance: Provenance,
    pub kind: MediaKind,
    /// The rendered size, kept when the pixels go.
    pub size: (u32, u32),
    pub warnings: Vec<Warning>,
    pub pixels: ItemPixels,
    /// What it was rendered from, and how.
    pub source: super::ItemSource,
    /// Where the render put its pixels, and the source's own size: what a
    /// zoomed lane plans its sharper pixels from.
    pub layout: Option<kettle_media::RenderLayout>,
    /// When the user last looked at it, on the process-wide view clock.
    viewed: u64,
    /// When it was last published, a replacement included, on that clock.
    published: u64,
    /// Whether the user has looked at it since it arrived or was replaced.
    seen: bool,
}

impl ShelfItem {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        id: u64,
        key: Option<ShelfKey>,
        title: String,
        provenance: Provenance,
        kind: MediaKind,
        warnings: Vec<Warning>,
        pixels: ImageData,
        source: super::ItemSource,
    ) -> Self {
        Self {
            id,
            generation: 0,
            key,
            title,
            provenance,
            kind,
            size: (pixels.width, pixels.height),
            warnings,
            pixels: ItemPixels::Ready(pixels),
            source,
            layout: None,
            viewed: 0,
            published: 0,
            seen: false,
        }
    }

    pub(crate) fn image(&self) -> Option<&ImageData> {
        match &self.pixels {
            ItemPixels::Ready(image) => Some(image),
            ItemPixels::Evicted => None,
        }
    }
}

/// What publishing an item did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Published {
    /// The id the item has on the shelf: a replaced item's, else its own.
    pub id: u64,
    /// The item that had to leave a full shelf, if any.
    pub dropped: Option<u64>,
}

#[derive(Debug, Default)]
pub(crate) struct Shelf {
    /// Newest first.
    items: Vec<ShelfItem>,
}

/// The next view stamp. One clock for every shelf, so the least recently
/// viewed item can be found across panes and windows.
fn tick() -> u64 {
    static CLOCK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    CLOCK.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1
}

impl Shelf {
    /// The item published last, a replacement by key included. The list
    /// keeps a replacement where its key was, so this is not always first.
    pub(crate) fn latest(&self) -> Option<&ShelfItem> {
        self.items.iter().max_by_key(|item| item.published)
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.items.len()
    }

    /// Newest first.
    pub(crate) fn items(&self) -> &[ShelfItem] {
        &self.items
    }

    pub(crate) fn get(&self, id: u64) -> Option<&ShelfItem> {
        self.items.iter().find(|item| item.id == id)
    }

    pub(crate) fn get_mut(&mut self, id: u64) -> Option<&mut ShelfItem> {
        self.items.iter_mut().find(|item| item.id == id)
    }

    /// Put `item` on the shelf. `visible` is the item the viewer shows,
    /// whose replacement is already seen; a full shelf never drops it, nor
    /// any item in `protected`, which are on screen elsewhere.
    pub(crate) fn publish(
        &mut self,
        mut item: ShelfItem,
        visible: Option<u64>,
        protected: &[u64],
    ) -> Published {
        let now = tick();
        item.viewed = now;
        item.published = now;
        if let Some(key) = item.key.as_ref()
            && let Some(slot) = self
                .items
                .iter_mut()
                .find(|existing| existing.key.as_ref() == Some(key))
        {
            item.id = slot.id;
            item.generation = slot.generation + 1;
            // A replacement lands in front of the user when its item is the
            // one on screen.
            item.seen = visible == Some(slot.id);
            *slot = item;
            return Published {
                id: slot.id,
                dropped: None,
            };
        }
        let id = item.id;
        self.items.insert(0, item);
        let dropped = if self.items.len() > MAX_SHELF_ITEMS {
            // The least recently viewed item goes, never the new one or the
            // viewer's. One on screen elsewhere is spared while another can
            // go instead; the shelf keeps its bound either way.
            let oldest = |spare_protected: bool| {
                self.items
                    .iter()
                    .enumerate()
                    .filter(|(_, item)| {
                        Some(item.id) != visible
                            && item.id != id
                            && !(spare_protected && protected.contains(&item.id))
                    })
                    .min_by_key(|(_, item)| item.viewed)
                    .map(|(index, _)| index)
            };
            oldest(true)
                .or_else(|| oldest(false))
                .map(|index| self.items.remove(index).id)
        } else {
            None
        };
        Published { id, dropped }
    }

    /// Note that the user looked at `id`.
    pub(crate) fn viewed(&mut self, id: u64) {
        if let Some(item) = self.items.iter_mut().find(|item| item.id == id) {
            item.viewed = tick();
            item.seen = true;
        }
    }

    /// Items the user has not looked at since they arrived or changed.
    pub(crate) fn unseen(&self) -> usize {
        self.items.iter().filter(|item| !item.seen).count()
    }

    /// The least recently viewed item still holding pixels and not on
    /// screen in any window (`visible`), and when it was viewed.
    pub(crate) fn eviction_candidate(&self, visible: &[u64]) -> Option<(u64, u64)> {
        self.items
            .iter()
            .filter(|item| !visible.contains(&item.id) && item.image().is_some())
            .min_by_key(|item| item.viewed)
            .map(|item| (item.viewed, item.id))
    }

    /// Release `id`'s pixels, and the bytes it kept to render again.
    pub(crate) fn evict_pixels(&mut self, id: u64) -> bool {
        match self.items.iter_mut().find(|item| item.id == id) {
            Some(item) if item.image().is_some() => {
                // The bytes it kept to render again go with the pixels.
                item.pixels = ItemPixels::Evicted;
                item.source.release();
                true
            }
            _ => false,
        }
    }
}

/// A shelf as `list_panes` reports it, newest first. Never pixels or
/// sources: what each item is, who sent it, and whether its pixels are held.
pub(crate) fn report(shelf: &Shelf) -> serde_json::Value {
    shelf
        .items()
        .iter()
        .map(|item| {
            let sender = match &item.provenance {
                Provenance::Verified | Provenance::User => serde_json::Value::Null,
                Provenance::Unverified(sender) => serde_json::json!({
                    "executable": sender.executable,
                    "pid": sender.pid,
                    "signer": sender.signer,
                }),
            };
            serde_json::json!({
                "item": item.id,
                "generation": item.generation,
                "title": item.title,
                "kind": item.kind.as_str(),
                "width": item.size.0,
                "height": item.size.1,
                "warnings": item.warnings.iter().map(|warning| warning.as_str()).collect::<Vec<_>>(),
                "verified": matches!(item.provenance, Provenance::Verified),
                "from_user": matches!(item.provenance, Provenance::User),
                "sender": sender,
                "pixels": if item.image().is_some() { "held" } else { "released" },
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: u64, key: Option<&str>) -> ShelfItem {
        ShelfItem::new(
            id,
            key.map(|key| ShelfKey::Explicit(key.into())),
            format!("item {id}"),
            Provenance::Verified,
            MediaKind::Raster,
            Vec::new(),
            ImageData::new(1, 1, vec![0, 0, 0, 255]).unwrap(),
            crate::media::ItemSource::sample(b"<svg/>"),
        )
    }

    /// A file's key is its native path: two paths that print alike are two
    /// items, a path and an explicit key that read alike are two, and the
    /// same path again replaces in place.
    #[test]
    fn native_path_keys_never_merge_by_their_printed_form() {
        let path =
            |bytes: &[u8]| ShelfKey::Path(kettle_media::NativePath::new(bytes.to_vec()).unwrap());
        let with = |id, key: ShelfKey| {
            let mut item = item(id, None);
            item.key = Some(key);
            item
        };
        let mut shelf = Shelf::default();
        // Both print as "/tmp/\u{FFFD}.png" on a Unix system.
        shelf.publish(with(1, path(b"/tmp/\xff.png")), None, &[]);
        shelf.publish(with(2, path(b"/tmp/\xfe.png")), None, &[]);
        shelf.publish(
            with(3, ShelfKey::Explicit("/tmp/\u{FFFD}.png".into())),
            None,
            &[],
        );
        assert_eq!(shelf.len(), 3);
        let again = shelf.publish(with(4, path(b"/tmp/\xff.png")), None, &[]);
        assert_eq!((again.id, shelf.len()), (1, 3));
    }

    /// Evicting an item's pixels gives back the bytes it kept to render
    /// again; a file item keeps its path.
    #[test]
    fn evicting_pixels_releases_kept_bytes() {
        let mut shelf = Shelf::default();
        shelf.publish(item(1, None), None, &[]);
        assert!(shelf.get(1).unwrap().source.text().is_some());
        assert!(shelf.evict_pixels(1));
        let evicted = shelf.get(1).unwrap();
        assert!(evicted.image().is_none());
        assert_eq!(evicted.source.text(), None);
    }

    fn ids(shelf: &Shelf) -> Vec<u64> {
        shelf.items().iter().map(|item| item.id).collect()
    }

    #[test]
    fn newest_first_and_a_key_replaces_in_place_keeping_its_id() {
        let mut shelf = Shelf::default();
        assert_eq!(shelf.publish(item(1, Some("plot")), None, &[]).id, 1);
        shelf.publish(item(2, None), None, &[]);
        let replaced = shelf.publish(item(3, Some("plot")), None, &[]);
        assert_eq!(
            replaced,
            Published {
                id: 1,
                dropped: None
            }
        );
        assert_eq!(ids(&shelf), [2, 1]);
        let plot = shelf.get(1).unwrap();
        assert_eq!((plot.generation, plot.title.as_str()), (1, "item 3"));
        // A replacement stays where its key was, yet it is the latest.
        assert_eq!(shelf.latest().map(|item| item.id), Some(1));
        // Keyless items never replace each other.
        shelf.publish(item(4, None), None, &[]);
        shelf.publish(item(5, None), None, &[]);
        assert_eq!(ids(&shelf), [5, 4, 2, 1]);
        assert_eq!(shelf.latest().map(|item| item.id), Some(5));
        assert_eq!(Shelf::default().latest().map(|item| item.id), None);
    }

    #[test]
    fn a_full_shelf_drops_the_least_recently_viewed_never_the_visible_one() {
        let mut shelf = Shelf::default();
        for id in 1..=MAX_SHELF_ITEMS as u64 {
            assert_eq!(shelf.publish(item(id, None), None, &[]).dropped, None);
        }
        // 1 is the oldest, but on screen; 2 was looked at just now.
        shelf.viewed(2);
        let published = shelf.publish(item(100, None), Some(1), &[]);
        assert_eq!(published.dropped, Some(3));
        assert_eq!(shelf.len(), MAX_SHELF_ITEMS);
        assert!(shelf.get(1).is_some() && shelf.get(2).is_some() && shelf.get(3).is_none());
        assert_eq!(shelf.items()[0].id, 100);
    }

    #[test]
    fn a_full_shelf_never_drops_an_item_whose_card_is_on_screen() {
        let mut shelf = Shelf::default();
        for id in 1..=MAX_SHELF_ITEMS as u64 {
            shelf.publish(item(id, None), None, &[]);
        }
        // Item 1 is the least recently viewed, but its card is painted.
        let published = shelf.publish(item(100, None), None, &[1]);
        assert_eq!(published.dropped, Some(2));
        assert!(shelf.get(1).is_some());
        // With every other item on screen, the shelf still keeps its bound:
        // the least recently viewed goes, never the viewer's item.
        let everything: Vec<u64> = shelf.items().iter().map(|item| item.id).collect();
        let viewer = everything[everything.len() - 1];
        let published = shelf.publish(item(101, None), Some(viewer), &everything);
        assert_eq!(shelf.len(), MAX_SHELF_ITEMS);
        assert!(
            published
                .dropped
                .is_some_and(|dropped| dropped != viewer && dropped != 101)
        );
        assert!(shelf.get(viewer).is_some());
    }

    #[test]
    fn items_are_unseen_until_viewed_and_again_when_replaced() {
        let mut shelf = Shelf::default();
        shelf.publish(item(1, Some("plot")), None, &[]);
        shelf.publish(item(2, None), None, &[]);
        assert_eq!(shelf.unseen(), 2);
        shelf.viewed(1);
        assert_eq!(shelf.unseen(), 1);
        shelf.publish(item(3, Some("plot")), None, &[]);
        assert_eq!(shelf.unseen(), 2, "a replaced item is new again");
    }

    #[test]
    fn replacing_the_item_on_screen_leaves_it_seen() {
        let mut shelf = Shelf::default();
        shelf.publish(item(1, Some("plot")), None, &[]);
        shelf.publish(item(2, Some("log")), None, &[]);
        shelf.viewed(1);
        shelf.viewed(2);
        shelf.publish(item(3, Some("plot")), Some(1), &[]);
        assert!(shelf.get(1).is_some_and(|item| item.seen));
        assert_eq!(shelf.unseen(), 0, "the user is looking at the replacement");
        shelf.publish(item(4, Some("log")), Some(1), &[]);
        assert_eq!(shelf.unseen(), 1, "a replacement off screen is new");
    }

    #[test]
    fn evicted_pixels_keep_the_item_and_spare_the_visible_one() {
        let mut shelf = Shelf::default();
        for id in 1..=3 {
            shelf.publish(item(id, None), None, &[]);
        }
        shelf.viewed(1);
        assert_eq!(shelf.eviction_candidate(&[2]).map(|(_, id)| id), Some(3));
        assert!(shelf.evict_pixels(3));
        assert!(!shelf.evict_pixels(3));
        assert!(shelf.get(3).unwrap().image().is_none());
        assert_eq!(shelf.eviction_candidate(&[2]).map(|(_, id)| id), Some(1));
        assert!(shelf.evict_pixels(1));
        assert_eq!(shelf.eviction_candidate(&[2]), None);
        assert_eq!(ids(&shelf), [3, 2, 1]);
    }

    #[test]
    fn a_signer_reads_as_its_certificate_name_or_apple() {
        use kettle_ctl::signing::Signature;
        let signature = |team: Option<&str>, authority: Option<&str>, apple: bool| Signature {
            identifier: "com.example.tool".into(),
            team: team.map(str::to_owned),
            authority: authority.map(str::to_owned),
            apple,
            executable: "/usr/local/bin/tool".into(),
        };
        let developer = "Developer ID Application: Anthropic PBC (Q6L2SF6YDW)";
        assert_eq!(
            signer_name(&signature(Some("Q6L2SF6YDW"), Some(developer), false)),
            "Anthropic PBC (Q6L2SF6YDW)"
        );
        assert_eq!(
            signer_name(&signature(None, Some("macOS Software Signing"), true)),
            "Apple"
        );
        assert_eq!(
            signer_name(&signature(None, Some(developer), false)),
            "Anthropic PBC (Q6L2SF6YDW)",
            "an older signature without a team is still its developer's, not Apple's"
        );
        assert_eq!(
            signer_name(&signature(Some("ABCDE12345"), None, false)),
            "ABCDE12345"
        );
        assert_eq!(
            signer_name(&signature(None, None, false)),
            "com.example.tool"
        );
    }

    #[test]
    fn the_report_names_items_and_senders_but_holds_no_pixels() {
        let mut shelf = Shelf::default();
        shelf.publish(item(1, Some("plot")), None, &[]);
        let mut unverified = item(2, None);
        unverified.provenance = Provenance::Unverified(UnverifiedSender {
            executable: Some("/usr/bin/tool".into()),
            pid: 77,
            signer: Some("Example Corp (ABCDE12345)".into()),
        });
        unverified.warnings = vec![Warning::FontFallback];
        shelf.publish(unverified, None, &[]);
        let mut pulled = item(3, None);
        pulled.provenance = Provenance::User;
        shelf.publish(pulled, None, &[]);
        shelf.evict_pixels(1);
        assert_eq!(
            report(&shelf),
            serde_json::json!([
                {"item": 3, "generation": 0, "title": "item 3", "kind": "raster",
                 "width": 1, "height": 1, "warnings": [], "verified": false,
                 "from_user": true, "sender": null, "pixels": "held"},
                {"item": 2, "generation": 0, "title": "item 2", "kind": "raster",
                 "width": 1, "height": 1, "warnings": ["font_fallback"], "verified": false,
                 "from_user": false,
                 "sender": {"executable": "/usr/bin/tool", "pid": 77,
                            "signer": "Example Corp (ABCDE12345)"}, "pixels": "held"},
                {"item": 1, "generation": 0, "title": "item 1", "kind": "raster",
                 "width": 1, "height": 1, "warnings": [], "verified": true,
                 "from_user": false, "sender": null, "pixels": "released"},
            ])
        );
    }
}

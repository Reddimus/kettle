//! Metadata-only admission planning for retained Kitty images.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Weak};

use crate::{GraphicsBudget, ImageData, PixelBuffer};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum QuotaIdentity {
    Named(u32),
    Anonymous(usize),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct QuotaKey {
    pub inactive: bool,
    pub image: QuotaIdentity,
}

/// One registry's allocation witness, without owning or copying its pixels.
#[derive(Clone, Debug)]
pub struct QuotaOwner {
    key: QuotaKey,
    allocation: usize,
    bytes: usize,
    pixels: Weak<PixelBuffer>,
    placed: bool,
    created: Option<u64>,
}

impl QuotaOwner {
    pub fn new(key: QuotaKey, image: &ImageData, placed: bool) -> Self {
        Self {
            key,
            allocation: image.allocation_key(),
            bytes: image.byte_len(),
            pixels: Arc::downgrade(&image.rgba),
            placed,
            created: image.kitty_creation_order(),
        }
    }
}

pub(crate) enum QuotaNeed {
    Upload {
        budget: GraphicsBudget,
        incoming_bytes: usize,
    },
    #[cfg(test)]
    Fixed(usize),
}

impl QuotaNeed {
    fn bytes(&self) -> usize {
        match self {
            Self::Upload {
                budget,
                incoming_bytes,
            } => budget
                .retained_cpu_bytes()
                .saturating_add(*incoming_bytes)
                .saturating_sub(budget.limits().retained_bytes),
            #[cfg(test)]
            Self::Fixed(bytes) => *bytes,
        }
    }
}

impl std::fmt::Debug for QuotaNeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("QuotaNeed").field(&self.bytes()).finish()
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct QuotaImage {
    pub key: QuotaKey,
    pub created: u64,
}

/// The decoder's request. A terminal adds its registry owners before planning.
#[derive(Debug)]
pub struct QuotaRequest {
    pub(crate) images: Vec<QuotaImage>,
    pub(crate) owners: Vec<QuotaOwner>,
    pub(crate) needed_bytes: QuotaNeed,
    pub(crate) needed_slot: bool,
    pub(crate) protected: QuotaKey,
}

impl QuotaRequest {
    /// Prefer unplaced images, then creation order. Return no plan when actual
    /// external owners prevent reclaiming enough bytes; preserve existing roots.
    pub fn plan(&self, extra: &[QuotaOwner]) -> Option<Vec<QuotaKey>> {
        struct Allocation<'a> {
            bytes: usize,
            pixels: &'a Weak<PixelBuffer>,
            key: QuotaKey,
            owned: usize,
            shared: bool,
        }
        let stored: HashSet<_> = self.images.iter().map(|image| image.key).collect();
        let mut images: HashMap<_, _> = self.images.iter().map(|i| (i.key, i.created)).collect();
        let mut placed = HashSet::new();
        let mut allocations = HashMap::<usize, Allocation<'_>>::new();
        for owner in self.owners.iter().chain(extra) {
            if owner.placed {
                placed.insert(owner.key);
            }
            if matches!(owner.key.image, QuotaIdentity::Anonymous(_))
                && let Some(created) = owner.created
            {
                images.entry(owner.key).or_insert(created);
            }
            let allocation = allocations
                .entry(owner.allocation)
                .or_insert_with(|| Allocation {
                    bytes: owner.bytes,
                    pixels: &owner.pixels,
                    key: owner.key,
                    owned: 0,
                    shared: false,
                });
            allocation.shared |= allocation.key != owner.key;
            allocation.owned += 1;
        }

        let mut reclaimable = HashMap::<QuotaKey, usize>::new();
        // Allocations shared between logical images are conservatively retained.
        // Multiple references within one image (root/frames/placements) count once.
        for allocation in allocations.values() {
            if !allocation.shared && allocation.pixels.strong_count() == allocation.owned {
                *reclaimable.entry(allocation.key).or_default() += allocation.bytes;
            }
        }
        let needed_bytes = self.needed_bytes.bytes();
        let mut candidates: Vec<_> = images.into_iter().collect();
        candidates.sort_unstable_by_key(|&(key, created)| (placed.contains(&key), created, key));
        let mut bytes = 0usize;
        let mut slot = !self.needed_slot;
        let mut selected = Vec::new();
        for (key, _) in candidates {
            if bytes >= needed_bytes && slot {
                break;
            }
            if key == self.protected {
                continue;
            }
            let releases_slot = !key.inactive && stored.contains(&key);
            let released = reclaimable.get(&key).copied().unwrap_or(0);
            if (bytes >= needed_bytes || released == 0) && (slot || !releases_slot) {
                continue;
            }
            bytes = bytes.checked_add(released)?;
            slot |= releases_slot;
            selected.push(key);
        }
        (bytes >= needed_bytes && slot).then_some(selected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GraphicsBudget, GraphicsLimits};

    fn key(id: u32, inactive: bool) -> QuotaKey {
        QuotaKey {
            inactive,
            image: QuotaIdentity::Named(id),
        }
    }

    fn pixel(budget: &GraphicsBudget, created: u64) -> ImageData {
        let mut image = ImageData::new_with_budget(1, 1, vec![1, 2, 3, 255], budget).unwrap();
        assert!(image.stamp_kitty_creation(created));
        image
    }

    fn request(roots: &[(QuotaKey, &ImageData)], bytes: usize, slot: bool) -> QuotaRequest {
        QuotaRequest {
            images: roots
                .iter()
                .map(|&(key, image)| QuotaImage {
                    key,
                    created: image.kitty_creation_order().unwrap(),
                })
                .collect(),
            owners: roots
                .iter()
                .map(|&(key, image)| QuotaOwner::new(key, image, false))
                .collect(),
            needed_bytes: QuotaNeed::Fixed(bytes),
            needed_slot: slot,
            protected: key(999, false),
        }
    }

    #[test]
    fn unplaced_roots_precede_older_visible_roots_in_creation_order() {
        let budget = GraphicsBudget::isolated(GraphicsLimits::default()).unwrap();
        let oldest = pixel(&budget, 1);
        let second = pixel(&budget, 2);
        let third = pixel(&budget, 3);
        let placement = oldest.clone();
        let request = request(
            &[
                (key(3, false), &third),
                (key(1, false), &oldest),
                (key(2, false), &second),
            ],
            4,
            false,
        );
        let visible = [QuotaOwner::new(key(1, false), &placement, true)];
        assert_eq!(request.plan(&visible), Some(vec![key(2, false)]));
        assert_eq!(request.plan(&visible), Some(vec![key(2, false)]));
        assert_eq!(budget.usage().0, 12, "planning does not drop pixel owners");
    }

    #[test]
    fn repeated_registry_owners_release_one_allocation() {
        let budget = GraphicsBudget::isolated(GraphicsLimits::default()).unwrap();
        let first = pixel(&budget, 1);
        let second = pixel(&budget, 2);
        let frame = first.clone();
        let request = request(
            &[(key(1, false), &first), (key(2, false), &second)],
            8,
            false,
        );
        assert_eq!(
            request.plan(&[QuotaOwner::new(key(1, false), &frame, false)]),
            Some(vec![key(1, false), key(2, false)]),
            "the root/frame alias cannot account for eight reclaimed bytes"
        );
    }

    #[test]
    fn animation_allocations_are_reclaimed_with_their_root() {
        let budget = GraphicsBudget::isolated(GraphicsLimits::default()).unwrap();
        let first = pixel(&budget, 1);
        let second = pixel(&budget, 2);
        let frame = pixel(&budget, 1);
        let request = request(
            &[(key(1, false), &first), (key(2, false), &second)],
            8,
            false,
        );
        assert_eq!(
            request.plan(&[QuotaOwner::new(key(1, false), &frame, false)]),
            Some(vec![key(1, false)])
        );
    }

    #[test]
    fn external_snapshot_requires_a_different_reclaimable_root() {
        let budget = GraphicsBudget::isolated(GraphicsLimits::default()).unwrap();
        let first = pixel(&budget, 1);
        let second = pixel(&budget, 2);
        let snapshot = first.clone();
        let request = request(
            &[(key(1, false), &first), (key(2, false), &second)],
            4,
            false,
        );
        assert_eq!(request.plan(&[]), Some(vec![key(2, false)]));
        assert_eq!(snapshot.rgba.as_slice(), &[1, 2, 3, 255]);
        assert_eq!(budget.usage().0, 8);
    }

    #[test]
    fn snapshot_created_after_metadata_keeps_its_allocation_charged() {
        let budget = GraphicsBudget::isolated(GraphicsLimits::default()).unwrap();
        let first = pixel(&budget, 1);
        let second = pixel(&budget, 2);
        let request = request(
            &[(key(1, false), &first), (key(2, false), &second)],
            4,
            false,
        );
        let snapshot = first.clone();
        assert_eq!(request.plan(&[]), Some(vec![key(2, false)]));
        assert_eq!(snapshot.rgba.as_slice(), &[1, 2, 3, 255]);
        assert_eq!(budget.usage().0, 8);
    }

    #[test]
    fn snapshot_released_after_metadata_does_not_block_reclamation() {
        let budget = GraphicsBudget::isolated(GraphicsLimits::default()).unwrap();
        let first = pixel(&budget, 1);
        let second = pixel(&budget, 2);
        let snapshot = first.clone();
        let request = request(
            &[(key(1, false), &first), (key(2, false), &second)],
            4,
            false,
        );
        drop(snapshot);
        assert_eq!(request.plan(&[]), Some(vec![key(1, false)]));
        assert_eq!(budget.usage().0, 8, "planning does not own or evict pixels");
    }

    #[test]
    fn insufficient_reclaimable_bytes_leave_all_roots_owned() {
        let budget = GraphicsBudget::isolated(GraphicsLimits::default()).unwrap();
        let first = pixel(&budget, 1);
        let second = pixel(&budget, 2);
        let snapshot = first.clone();
        let request = request(
            &[(key(1, false), &first), (key(2, false), &second)],
            8,
            false,
        );
        assert_eq!(request.plan(&[]), None);
        assert_eq!(snapshot.rgba.as_slice(), &[1, 2, 3, 255]);
        assert_eq!(budget.usage().0, 8);
    }

    #[test]
    fn active_slot_pressure_preserves_inactive_roots() {
        let budget = GraphicsBudget::isolated(GraphicsLimits::default()).unwrap();
        let inactive = pixel(&budget, 1);
        let active = pixel(&budget, 2);
        let request = request(
            &[(key(1, true), &inactive), (key(1, false), &active)],
            0,
            true,
        );
        assert_eq!(request.plan(&[]), Some(vec![key(1, false)]));
    }

    #[test]
    fn combined_pressure_reclaims_inactive_bytes_and_an_active_slot() {
        let budget = GraphicsBudget::isolated(GraphicsLimits::default()).unwrap();
        let inactive = pixel(&budget, 1);
        let active = pixel(&budget, 2);
        let request = request(
            &[(key(1, true), &inactive), (key(1, false), &active)],
            8,
            true,
        );
        assert_eq!(request.plan(&[]), Some(vec![key(1, true), key(1, false)]));
        assert_eq!(budget.usage().0, 8, "planning leaves both roots owned");
    }

    #[test]
    fn inactive_roots_cannot_supply_an_active_slot() {
        let budget = GraphicsBudget::isolated(GraphicsLimits::default()).unwrap();
        let inactive = pixel(&budget, 1);
        let request = request(&[(key(1, true), &inactive)], 4, true);
        assert_eq!(request.plan(&[]), None);
        assert_eq!(budget.usage().0, 4);
    }

    #[test]
    fn anonymous_kitty_pixels_supply_bytes_but_not_stored_slots() {
        let budget = GraphicsBudget::isolated(GraphicsLimits::default()).unwrap();
        let anonymous = pixel(&budget, 1);
        let anonymous_key = QuotaKey {
            inactive: false,
            image: QuotaIdentity::Anonymous(anonymous.allocation_key()),
        };
        let owners = [QuotaOwner::new(anonymous_key, &anonymous, true)];
        assert_eq!(
            request(&[], 4, false).plan(&owners),
            Some(vec![anonymous_key])
        );
        assert_eq!(request(&[], 4, true).plan(&owners), None);
        let plain = ImageData::new_with_budget(1, 1, vec![4, 5, 6, 255], &budget).unwrap();
        let plain_key = QuotaKey {
            inactive: false,
            image: QuotaIdentity::Anonymous(plain.allocation_key()),
        };
        assert_eq!(
            request(&[], 4, false).plan(&[QuotaOwner::new(plain_key, &plain, true)]),
            None
        );
    }

    #[test]
    fn protected_and_shared_logical_roots_are_not_fake_reclamation() {
        let budget = GraphicsBudget::isolated(GraphicsLimits::default()).unwrap();
        let first = pixel(&budget, 1);
        let second = first.clone();
        assert_eq!(
            request(
                &[(key(1, false), &first), (key(2, false), &second)],
                4,
                false
            )
            .plan(&[]),
            None
        );
        let mut protected = request(
            &[(key(1, false), &first), (key(2, false), &second)],
            0,
            true,
        );
        protected.protected = key(1, false);
        assert_eq!(protected.plan(&[]), Some(vec![key(2, false)]));
    }
}

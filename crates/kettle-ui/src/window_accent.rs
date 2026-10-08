//! Automatic accent ownership survives a window being checked out of the map.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use kettle_config::Rgb;

#[derive(Default)]
pub(crate) struct AccentRegistry {
    claims: RefCell<Vec<Weak<Cell<Rgb>>>>,
}

pub(crate) struct AccentClaim(Rc<Cell<Rgb>>);

impl AccentClaim {
    pub(crate) fn set_color(&self, color: Rgb) {
        self.0.set(color);
    }
}

impl AccentRegistry {
    pub(crate) fn claim(
        &self,
        pool: &[Rgb],
        seed: u64,
        external_colors: &[Rgb],
        avoid: Option<Rgb>,
    ) -> Option<(usize, AccentClaim)> {
        if pool.is_empty() {
            return None;
        }
        let mut claims = self.claims.borrow_mut();
        let mut in_use = external_colors.to_vec();
        claims.retain(|weak| {
            if let Some(claim) = weak.upgrade() {
                in_use.push(claim.get());
                true
            } else {
                false
            }
        });
        let slot = pick_accent_slot_avoiding(pool, seed, &in_use, avoid);
        let claim = Rc::new(Cell::new(pool[slot]));
        claims.push(Rc::downgrade(&claim));
        Some((slot, AccentClaim(claim)))
    }
}

#[cfg(test)]
pub(crate) fn pick_accent_slot(pool: &[Rgb], seed: u64, in_use: &[Rgb]) -> usize {
    pick_accent_slot_avoiding(pool, seed, in_use, None)
}

/// Prefer an unused hue, then the least-used eligible hue. A torn window avoids
/// its opener's actual color whenever the configured pool has another color.
fn pick_accent_slot_avoiding(pool: &[Rgb], seed: u64, in_use: &[Rgb], avoid: Option<Rgb>) -> usize {
    if pool.is_empty() {
        return 0;
    }
    let avoid = avoid.filter(|color| pool.iter().any(|candidate| candidate != color));
    let start = (seed % pool.len() as u64) as usize;
    (0..pool.len())
        .map(|offset| (start + offset) % pool.len())
        .filter(|&slot| Some(pool[slot]) != avoid)
        .min_by_key(|&slot| in_use.iter().filter(|&&color| color == pool[slot]).count())
        .unwrap_or(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pool() -> [Rgb; 3] {
        [Rgb::new(1, 2, 3), Rgb::new(4, 5, 6), Rgb::new(7, 8, 9)]
    }

    #[test]
    fn checked_out_window_claim_dedupes_without_presence() {
        let registry = AccentRegistry::default();
        let pool = pool();
        // The opener keeps its claim while outside App.windows.
        let (first_slot, opener) = registry.claim(&pool, 0, &[], None).unwrap();
        let (second_slot, _torn) = registry.claim(&pool, 0, &[], None).unwrap();
        assert_eq!(first_slot, 0);
        assert_eq!(second_slot, 1);
        drop(opener);
    }

    #[test]
    fn closed_or_pinned_window_releases_and_prunes_its_claim() {
        let registry = AccentRegistry::default();
        let pool = pool();
        let (_, first) = registry.claim(&pool, 0, &[], None).unwrap();
        drop(first);
        let (slot, _replacement) = registry.claim(&pool, 0, &[], None).unwrap();
        assert_eq!(slot, 0);
        assert_eq!(registry.claims.borrow().len(), 1);
    }

    #[test]
    fn theme_change_updates_color_seen_by_later_windows() {
        let registry = AccentRegistry::default();
        let pool = pool();
        let (_, first) = registry.claim(&pool, 0, &[], None).unwrap();
        first.set_color(pool[1]);
        let (slot, _second) = registry.claim(&pool, 1, &[], None).unwrap();
        assert_eq!(slot, 2);
    }

    #[test]
    fn combines_external_claims_and_keeps_full_pool_fallback() {
        let registry = AccentRegistry::default();
        let pool = pool();
        let (slot, _first) = registry.claim(&pool, 0, &[pool[0]], None).unwrap();
        assert_eq!(slot, 1);
        let (slot, _second) = registry.claim(&pool, 0, &pool, None).unwrap();
        assert_eq!(slot, 0);
        assert!(registry.claim(&[], 0, &[], None).is_none());
    }

    #[test]
    fn saturated_pool_tear_off_avoids_the_opener_color() {
        let registry = AccentRegistry::default();
        let pool = pool();
        let (opener, _first) = registry.claim(&pool, 0, &[], None).unwrap();
        let (_, _second) = registry.claim(&pool, 0, &[], None).unwrap();
        let (_, _third) = registry.claim(&pool, 0, &[], None).unwrap();
        let (child, _child) = registry.claim(&pool, 0, &[], Some(pool[opener])).unwrap();
        assert_ne!(pool[child], pool[opener]);
    }

    #[test]
    fn saturated_pool_prefers_the_least_used_color() {
        let pool = pool();
        assert_eq!(
            pick_accent_slot(&pool, 0, &[pool[0], pool[0], pool[0], pool[1], pool[2]]),
            1
        );
    }
    #[test]
    fn opener_exclusion_uses_color_not_slot_and_keeps_seed_ties() {
        let pool = pool();
        let duplicate = [pool[0], pool[0], pool[1]];
        assert_eq!(
            pick_accent_slot_avoiding(&duplicate, 0, &duplicate, Some(pool[0])),
            2
        );
        assert_eq!(pick_accent_slot_avoiding(&pool, 2, &pool, Some(pool[0])), 2);
        assert_eq!(pick_accent_slot_avoiding(&pool, 0, &pool, Some(pool[0])), 1);
    }

    #[test]
    fn single_hue_and_empty_pools_keep_configured_behavior() {
        let color = pool()[0];
        assert_eq!(
            pick_accent_slot_avoiding(&[color, color], 1, &[color], Some(color)),
            1
        );
        assert_eq!(pick_accent_slot_avoiding(&[], 7, &[], Some(color)), 0);
    }
}

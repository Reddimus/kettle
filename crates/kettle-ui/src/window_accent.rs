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
        let slot = pick_accent_slot(pool, seed, &in_use);
        let claim = Rc::new(Cell::new(pool[slot]));
        claims.push(Rc::downgrade(&claim));
        Some((slot, AccentClaim(claim)))
    }
}

/// Walk from the project's seed; a fully occupied pool reuses that slot.
pub(crate) fn pick_accent_slot(pool: &[Rgb], seed: u64, in_use: &[Rgb]) -> usize {
    if pool.is_empty() {
        return 0;
    }
    let start = (seed % pool.len() as u64) as usize;
    (0..pool.len())
        .map(|i| (start + i) % pool.len())
        .find(|&i| !in_use.contains(&pool[i]))
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
        let (first_slot, opener) = registry.claim(&pool, 0, &[]).unwrap();
        let (second_slot, _torn) = registry.claim(&pool, 0, &[]).unwrap();
        assert_eq!(first_slot, 0);
        assert_eq!(second_slot, 1);
        drop(opener);
    }

    #[test]
    fn closed_or_pinned_window_releases_and_prunes_its_claim() {
        let registry = AccentRegistry::default();
        let pool = pool();
        let (_, first) = registry.claim(&pool, 0, &[]).unwrap();
        drop(first);
        let (slot, _replacement) = registry.claim(&pool, 0, &[]).unwrap();
        assert_eq!(slot, 0);
        assert_eq!(registry.claims.borrow().len(), 1);
    }

    #[test]
    fn theme_change_updates_color_seen_by_later_windows() {
        let registry = AccentRegistry::default();
        let pool = pool();
        let (_, first) = registry.claim(&pool, 0, &[]).unwrap();
        first.set_color(pool[1]);
        let (slot, _second) = registry.claim(&pool, 1, &[]).unwrap();
        assert_eq!(slot, 2);
    }

    #[test]
    fn combines_external_claims_and_keeps_full_pool_fallback() {
        let registry = AccentRegistry::default();
        let pool = pool();
        let (slot, _first) = registry.claim(&pool, 0, &[pool[0]]).unwrap();
        assert_eq!(slot, 1);
        let (slot, _second) = registry.claim(&pool, 0, &pool).unwrap();
        assert_eq!(slot, 0);
        assert!(registry.claim(&[], 0, &[]).is_none());
    }
}

//! Automatic accent ownership survives a window being checked out of the map.

use std::cell::{Cell, RefCell};
use std::rc::{Rc, Weak};

use kettle_config::Rgb;

/// The accent for pool `slot` under `cfg`, or the configured accent when
/// automatic accents are off or pinned.
pub(crate) fn resolve_accent(cfg: &kettle_config::Config, slot: Option<usize>) -> Rgb {
    if cfg.accent_auto
        && cfg.accent_color.is_none()
        && let Some(slot) = slot
    {
        let pool = kettle_config::peacock_pool(&cfg.theme);
        if !pool.is_empty() {
            return pool[slot % pool.len()];
        }
    }
    cfg.resolved_accent(&cfg.theme)
}

/// Peacock: `#rrggbb` for the presence registry's wire format.
pub(crate) fn rgb_hex(c: Rgb) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r, c.g, c.b)
}

/// Show the window's accent claim, or the configured accent without one, on
/// its renderer. The claim is the only source of truth: it can change while a
/// window has no renderer, so every renderer installed for the window must be
/// given it again.
pub(crate) fn apply_accent(ws: &mut crate::window_state::WindowState) {
    let color = ws.accent.as_ref().map(|accent| accent.color);
    if let Some(r) = ws.renderer.as_mut() {
        r.set_accent_override(color);
    }
}

/// Keep a window's accent claim current. A pinned or disabled accent drops
/// the claim; a theme or palette change re-resolves the same pool slot
/// against the new pool, so every window shifts consistently, and updates
/// the local and cross-process claims. A window without a claim is left for
/// the caller to assign.
pub(crate) fn refresh_accent(
    ws: &mut crate::window_state::WindowState,
    cfg: &kettle_config::Config,
) {
    if cfg.accent_color.is_some() || !cfg.accent_auto {
        if ws.accent.take().is_some() {
            apply_accent(ws);
        }
        return;
    }
    let candidates = kettle_config::peacock_candidates(&cfg.theme);
    let Some(acc) = &mut ws.accent else {
        return;
    };
    if acc.theme_name == cfg.theme_name && acc.pool_candidates == candidates {
        return;
    }
    let color = resolve_accent(cfg, Some(acc.slot));
    acc.theme_name = cfg.theme_name.clone();
    acc.pool_candidates = candidates;
    if color != acc.color {
        acc.color = color;
        acc.local_claim.set_color(color);
        if let Some(g) = acc.presence.as_mut() {
            g.set_rgb(&rgb_hex(color));
        }
    }
    apply_accent(ws);
}

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

    fn claimed_window(
        registry: &AccentRegistry,
        cfg: &kettle_config::Config,
        seed: u64,
    ) -> crate::window_state::WindowState {
        let mut ws = crate::window_state::WindowState::new(1, false, crate::mux::Mux::new());
        let pool = kettle_config::peacock_pool(&cfg.theme);
        let (slot, local_claim) = registry.claim(&pool, seed, &[], None).unwrap();
        ws.accent = Some(crate::window_state::WindowAccent {
            local_claim,
            color: pool[slot],
            slot,
            theme_name: cfg.theme_name.clone(),
            pool_candidates: kettle_config::peacock_candidates(&cfg.theme),
            presence: None,
        });
        ws
    }

    #[test]
    fn same_name_palette_reload_refreshes_the_source_claim_before_tearing() {
        let registry = AccentRegistry::default();
        let before = kettle_config::Config::parse_text("theme = Dracula");
        let mut source = claimed_window(&registry, &before, 0);
        let old = source.accent.as_ref().unwrap().color;
        let after = kettle_config::Config::parse_text("theme = Dracula\npalette = 4=#102030");
        assert_eq!(before.theme_name, after.theme_name);
        assert_ne!(old, resolve_accent(&after, Some(0)));
        refresh_accent(&mut source, &after);
        let source = source.accent.as_ref().unwrap();
        assert_eq!(source.slot, 0);
        assert_eq!(source.color, Rgb::new(0x10, 0x20, 0x30));
        assert_eq!(source.local_claim.0.get(), source.color);
        let pool = kettle_config::peacock_pool(&after.theme);
        let (child, _claim) = registry.claim(&pool, 0, &[], Some(source.color)).unwrap();
        assert_ne!(pool[child], source.color);
        assert_ne!(child, 0);
    }

    #[test]
    fn same_name_palette_shrink_preserves_slot_modulo_and_live_claim() {
        let registry = AccentRegistry::default();
        let before = kettle_config::Config::parse_text("theme = Dracula");
        let mut source = claimed_window(&registry, &before, 7);
        let slot = source.accent.as_ref().unwrap().slot;
        let mut text = String::from("theme = Dracula\n");
        for index in 0..16 {
            text.push_str(&format!("palette = {index}=#102030\n"));
        }
        let after = kettle_config::Config::parse_text(&text);
        assert_eq!(before.theme_name, after.theme_name);
        let pool = kettle_config::peacock_pool(&after.theme);
        assert_eq!(pool, [Rgb::new(0x10, 0x20, 0x30)]);
        refresh_accent(&mut source, &after);
        let source = source.accent.as_ref().unwrap();
        assert_eq!(source.slot, slot);
        assert_eq!(source.color, pool[slot % pool.len()]);
        assert_eq!(source.local_claim.0.get(), source.color);
    }

    #[test]
    fn theme_change_before_redraw_refreshes_the_tearoff_reservation() {
        let registry = AccentRegistry::default();
        let mut cfg = kettle_config::Config {
            theme_name: "before".into(),
            ..kettle_config::Config::default()
        };
        cfg.theme.accent = Rgb::new(1, 2, 3);
        cfg.theme.palette.fill(cfg.theme.accent);
        let mut source = claimed_window(&registry, &cfg, 0);
        let stale = source.accent.as_ref().unwrap().color;
        cfg.theme_name = "after".into();
        cfg.theme.accent = Rgb::new(4, 5, 6);
        cfg.theme.palette.fill(Rgb::new(7, 8, 9));
        let next = kettle_config::peacock_pool(&cfg.theme);
        // Reproduce the interleaving: stale ownership cannot reserve new hues.
        let (bad, bad_claim) = registry.claim(&next, 0, &[], Some(stale)).unwrap();
        assert_eq!(next[bad], resolve_accent(&cfg, Some(0)));
        drop(bad_claim);
        refresh_accent(&mut source, &cfg);
        let current = source.accent.as_ref().unwrap();
        assert_eq!(current.theme_name, cfg.theme_name);
        assert_eq!(current.slot, 0);
        assert_eq!(current.color, next[0]);
        // The actual claim, not just the exclusion argument, was refreshed.
        let (later, observer) = registry.claim(&next, 0, &[], None).unwrap();
        assert_eq!(later, 1);
        drop(observer);
        let (child, _claim) = registry.claim(&next, 0, &[], Some(current.color)).unwrap();
        assert_ne!(next[child], current.color);
    }

    #[test]
    fn tearoff_refresh_preserves_slots_in_a_shorter_theme_pool() {
        let registry = AccentRegistry::default();
        let mut cfg = kettle_config::Config::default();
        let mut source = claimed_window(&registry, &cfg, u64::MAX);
        let slot = source.accent.as_ref().unwrap().slot;
        cfg.theme_name = "two hues".into();
        cfg.theme.accent = Rgb::new(11, 22, 33);
        cfg.theme.palette.fill(Rgb::new(44, 55, 66));
        refresh_accent(&mut source, &cfg);
        let pool = kettle_config::peacock_pool(&cfg.theme);
        let current = source.accent.as_ref().unwrap();
        assert_eq!(current.slot, slot);
        assert_eq!(current.color, pool[slot % pool.len()]);
        let (child, _claim) = registry
            .claim(&pool, u64::MAX, &[], Some(current.color))
            .unwrap();
        assert_ne!(pool[child], current.color);
        cfg.accent_color = Some(Rgb::new(5, 15, 25));
        refresh_accent(&mut source, &cfg);
        assert!(source.accent.is_none());
        assert_eq!(resolve_accent(&cfg, Some(slot)), Rgb::new(5, 15, 25));
        assert_eq!(
            registry
                .claims
                .borrow()
                .iter()
                .filter(|claim| claim.upgrade().is_some())
                .count(),
            1
        );
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

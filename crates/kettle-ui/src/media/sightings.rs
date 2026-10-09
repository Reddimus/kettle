//! Where each inline card is on screen and since when. A press on a card
//! that only just appeared stays ordinary input, so a card that scrolls
//! under the pointer cannot take a click meant for text. Each placement also
//! gets an opaque instance number, which names it to assistive technology
//! without exposing its nonce.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use kettle_core::InlineNonce;
use kettle_render::PaintedCard;

/// How long a card must have been where it is drawn before a press on it is
/// the card's.
pub(crate) const CARD_SETTLE: Duration = Duration::from_millis(500);

/// Instance numbers stay below this, so they fit the accessibility node
/// namespace beside the bit that marks it.
pub(crate) const MAX_CARD_INSTANCE: u64 = 1 << 57;

/// Where a card is on screen: its pane, its nonce and the bits of its rect.
/// Two printed copies of one card are two spots, and a card that moved is at
/// a new one.
type CardSpot = (u64, InlineNonce, [u32; 4]);

fn card_spot(card: &PaintedCard) -> CardSpot {
    (card.pane, card.nonce, card.rect.map(f32::to_bits))
}

#[derive(Clone, Copy, Debug)]
struct Sighting {
    since: Instant,
    /// `None` once every instance number is spent, which leaves the card
    /// clickable but unnamed to assistive technology.
    instance: Option<u64>,
}

/// The cards on screen, as of the last presented frame.
#[derive(Debug, Default)]
pub(crate) struct CardSightings {
    spots: HashMap<CardSpot, Sighting>,
    /// The last instance number given out.
    last: u64,
}

impl CardSightings {
    /// Note when each card on screen first appeared where it is: a card that
    /// moved counts as new, with a new instance, and one no longer on screen
    /// is forgotten. Says whether the set of cards changed.
    pub(crate) fn note(&mut self, painted: &[PaintedCard], now: Instant) -> bool {
        let before = self.spots.len();
        self.spots
            .retain(|spot, _| painted.iter().any(|card| card_spot(card) == *spot));
        let mut changed = self.spots.len() != before;
        for card in painted {
            if let std::collections::hash_map::Entry::Vacant(entry) =
                self.spots.entry(card_spot(card))
            {
                let instance = self.last.checked_add(1).filter(|n| *n < MAX_CARD_INSTANCE);
                if let Some(instance) = instance {
                    self.last = instance;
                }
                entry.insert(Sighting {
                    since: now,
                    instance,
                });
                changed = true;
            }
        }
        changed
    }

    /// The card a press on `card` is for: one on screen there that has been
    /// in place for `CARD_SETTLE`.
    pub(crate) fn settled(
        &self,
        card: Option<PaintedCard>,
        now: Instant,
    ) -> Option<(u64, InlineNonce)> {
        let card = card?;
        let seen = self.spots.get(&card_spot(&card))?;
        (now.saturating_duration_since(seen.since) >= CARD_SETTLE)
            .then_some((card.pane, card.nonce))
    }

    /// The instance number of `card`'s placement, if it is on screen.
    pub(crate) fn instance(&self, card: &PaintedCard) -> Option<u64> {
        self.spots.get(&card_spot(card))?.instance
    }

    /// The pane and nonce of the placement `instance` names, while it is on
    /// screen. An instance is never reused, so a stale one names nothing.
    pub(crate) fn by_instance(&self, instance: u64) -> Option<(u64, InlineNonce)> {
        self.spots
            .iter()
            .find(|(_, seen)| seen.instance == Some(instance))
            .map(|((pane, nonce, _), _)| (*pane, *nonce))
    }

    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.spots.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(pane: u64, nonce: InlineNonce, rect: [f32; 4]) -> PaintedCard {
        PaintedCard { pane, nonce, rect }
    }

    /// A press is a card's only once the card has been where it is drawn
    /// for half a second; one that moved, or just appeared, is new again,
    /// and one no longer drawn is forgotten.
    #[test]
    fn a_card_takes_a_press_only_once_it_has_settled() {
        let nonce = InlineNonce::new([1, 2, 3, 4, 5, 6]).unwrap();
        let here = card(7, nonce, [40.0, 16.0, 96.0, 48.0]);
        let start = Instant::now();
        let ms = Duration::from_millis;
        let mut sightings = CardSightings::default();
        assert!(sightings.note(&[here], start));
        assert_eq!(sightings.settled(Some(here), start + ms(499)), None);
        assert_eq!(
            sightings.settled(Some(here), start + ms(500)),
            Some((7, nonce))
        );
        // A later frame in the same place keeps the first sighting.
        assert!(!sightings.note(&[here], start + ms(300)));
        assert_eq!(
            sightings.settled(Some(here), start + ms(500)),
            Some((7, nonce))
        );
        // Moved, it is new again from that frame.
        let moved = card(7, nonce, [40.0, 32.0, 96.0, 48.0]);
        assert!(sightings.note(&[moved], start + ms(600)));
        assert_eq!(sightings.settled(Some(moved), start + ms(1000)), None);
        assert_eq!(
            sightings.settled(Some(moved), start + ms(1100)),
            Some((7, nonce))
        );
        // A sighting is for the place it was seen.
        assert_eq!(sightings.settled(Some(here), start + ms(2000)), None);
        // No longer drawn, it is forgotten.
        assert!(sightings.note(&[], start + ms(1200)));
        assert!(sightings.is_empty());
        assert_eq!(sightings.settled(None, start), None);
        // Two printed copies of one card settle apart, each where it is, and
        // frames showing both keep both first sightings.
        let below = card(7, nonce, [40.0, 96.0, 96.0, 48.0]);
        sightings.note(&[here, below], start);
        sightings.note(&[here, below], start + ms(400));
        assert_eq!(
            sightings.settled(Some(here), start + ms(500)),
            Some((7, nonce))
        );
        assert_eq!(
            sightings.settled(Some(below), start + ms(500)),
            Some((7, nonce))
        );
    }

    /// Each placement has its own instance, a moved or returning card a new
    /// one, and a stale instance names nothing, so assistive technology can
    /// never open a card other than the one it named.
    #[test]
    fn each_placement_is_its_own_instance_and_none_is_reused() {
        let nonce = InlineNonce::new([1, 2, 3, 4, 5, 6]).unwrap();
        let here = card(7, nonce, [40.0, 16.0, 96.0, 48.0]);
        let below = card(7, nonce, [40.0, 96.0, 96.0, 48.0]);
        let start = Instant::now();
        let mut sightings = CardSightings::default();
        sightings.note(&[here, below], start);
        let (first, second) = (
            sightings.instance(&here).unwrap(),
            sightings.instance(&below).unwrap(),
        );
        assert_ne!(first, second);
        assert_eq!(sightings.by_instance(first), Some((7, nonce)));
        sightings.note(&[below], start);
        assert_eq!(sightings.by_instance(first), None, "gone");
        assert_eq!(sightings.instance(&below), Some(second), "kept");
        sightings.note(&[here, below], start);
        let returned = sightings.instance(&here).unwrap();
        assert!(returned != first && returned != second, "never reused");
        assert_eq!(sightings.by_instance(first), None);

        // Spent numbers leave a card unnamed rather than wrapping.
        let mut spent = CardSightings {
            last: MAX_CARD_INSTANCE - 1,
            ..CardSightings::default()
        };
        spent.note(&[here], start);
        assert_eq!(spent.instance(&here), None);
        assert_eq!(
            spent.settled(Some(here), start + CARD_SETTLE),
            Some((7, nonce))
        );
    }
}

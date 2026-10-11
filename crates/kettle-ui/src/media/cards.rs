//! The inline cards Kettle registered, across every pane and window: which
//! pane and shelf item each shows, which harness process owns it, and the
//! limits that keep one harness from flooding the rest. The renderer only
//! paints what a pane's `InlineCards` holds; this ledger decides what that is.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use kettle_core::InlineNonce;
use kettle_ctl::process::ProcessIdentity;

/// Most live cards one harness process owns at once, across all panes.
pub(crate) const MAX_HARNESS_CARDS: usize = 32;
/// Most cards one harness process registers in any second.
pub(crate) const HARNESS_CARDS_PER_SECOND: usize = 4;
/// Attempts at a nonce no live card holds, before a push goes to the shelf
/// alone.
const NONCE_ATTEMPTS: usize = 4;
/// The radix of a nonce digit, one of 108 marks.
const RADIX: u8 = 108;

/// One registered card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CardRecord {
    pub pane: u64,
    /// The shelf item it shows, at the generation it was registered for.
    pub item: (u64, u64),
    /// The harness process that asked for it.
    pub owner: ProcessIdentity,
}

/// Why a harness may not register a card now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CardLimit {
    /// It owns as many live cards as it may.
    Owned,
    /// It registered as many as it may this second.
    Rate,
}

#[derive(Debug, Default)]
pub(crate) struct CardLedger {
    cards: HashMap<InlineNonce, CardRecord>,
    /// Recent registrations, oldest first, at most one second's worth per
    /// owner.
    recent: VecDeque<(ProcessIdentity, Instant)>,
}

impl CardLedger {
    /// Whether `owner` may register another card at `now`.
    pub(crate) fn admit(&mut self, owner: ProcessIdentity, now: Instant) -> Result<(), CardLimit> {
        while self
            .recent
            .front()
            .is_some_and(|(_, at)| now.saturating_duration_since(*at) >= Duration::from_secs(1))
        {
            self.recent.pop_front();
        }
        if self
            .cards
            .values()
            .filter(|card| card.owner == owner)
            .count()
            >= MAX_HARNESS_CARDS
        {
            return Err(CardLimit::Owned);
        }
        if self.recent.iter().filter(|(who, _)| *who == owner).count() >= HARNESS_CARDS_PER_SECOND {
            return Err(CardLimit::Rate);
        }
        Ok(())
    }

    /// A nonce no live card holds, from `random`'s bytes: each digit takes a
    /// byte below the largest multiple of 108, so every digit is equally
    /// likely. `None` when `random` fails or every attempt collides.
    pub(crate) fn mint(&self, mut random: impl FnMut(&mut [u8]) -> bool) -> Option<InlineNonce> {
        // 216 is the largest multiple of 108 a byte holds.
        const LIMIT: u8 = RADIX * 2;
        for _ in 0..NONCE_ATTEMPTS {
            let mut digits = [0u8; 6];
            let mut filled = 0;
            let mut pool = [0u8; 32];
            while filled < digits.len() {
                if !random(&mut pool) {
                    return None;
                }
                for &byte in pool.iter().filter(|&&byte| byte < LIMIT) {
                    if filled == digits.len() {
                        break;
                    }
                    digits[filled] = byte % RADIX;
                    filled += 1;
                }
            }
            let nonce = InlineNonce::new(digits)?;
            if !self.cards.contains_key(&nonce) {
                return Some(nonce);
            }
        }
        None
    }

    /// Note a registered card, charged to its owner at `now`.
    pub(crate) fn record(&mut self, nonce: InlineNonce, card: CardRecord, now: Instant) {
        self.recent.push_back((card.owner, now));
        self.cards.insert(nonce, card);
    }

    /// The shelf item a card shows, if it is live.
    pub(crate) fn item(&self, pane: u64, nonce: InlineNonce) -> Option<u64> {
        self.cards
            .get(&nonce)
            .filter(|card| card.pane == pane)
            .map(|card| card.item.0)
    }

    /// Retire every card `retire` names, returning each one's pane and nonce
    /// so its pane forgets it too.
    pub(crate) fn retire(
        &mut self,
        mut retire: impl FnMut(&CardRecord) -> bool,
    ) -> Vec<(u64, InlineNonce)> {
        let mut retired = Vec::new();
        self.cards.retain(|&nonce, card| {
            if retire(card) {
                retired.push((card.pane, nonce));
                false
            } else {
                true
            }
        });
        retired
    }

    /// The distinct owners of live cards, to check whether each still runs.
    pub(crate) fn owners(&self) -> Vec<ProcessIdentity> {
        let mut owners: Vec<ProcessIdentity> = self.cards.values().map(|card| card.owner).collect();
        owners.sort_by_key(|owner| (owner.pid(), owner.start()));
        owners.dedup();
        owners
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.cards.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.cards.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn owner(pid: u32) -> ProcessIdentity {
        ProcessIdentity::new_for_tests(pid, 1)
    }

    fn card(pane: u64, owner_pid: u32) -> CardRecord {
        CardRecord {
            pane,
            item: (1, 0),
            owner: owner(owner_pid),
        }
    }

    fn nonce(n: u8) -> InlineNonce {
        InlineNonce::new([0, 0, 0, 0, 0, n]).unwrap()
    }

    #[test]
    fn a_harness_owns_at_most_its_share_and_registers_at_most_four_a_second() {
        let mut ledger = CardLedger::default();
        let start = Instant::now();
        for n in 0..HARNESS_CARDS_PER_SECOND as u8 {
            assert_eq!(ledger.admit(owner(7), start), Ok(()));
            ledger.record(nonce(n), card(1, 7), start);
        }
        assert_eq!(ledger.admit(owner(7), start), Err(CardLimit::Rate));
        assert_eq!(
            ledger.admit(owner(8), start),
            Ok(()),
            "another harness has its own"
        );
        let later = start + Duration::from_secs(1);
        assert_eq!(ledger.admit(owner(7), later), Ok(()));
        let mut at = later;
        for n in HARNESS_CARDS_PER_SECOND as u8..MAX_HARNESS_CARDS as u8 {
            at += Duration::from_secs(1);
            ledger.record(nonce(n), card(1 + u64::from(n % 3), 7), at);
        }
        at += Duration::from_secs(1);
        assert_eq!(
            ledger.admit(owner(7), at),
            Err(CardLimit::Owned),
            "counted across panes"
        );
        ledger.retire(|card| card.pane == 1);
        assert_eq!(ledger.admit(owner(7), at), Ok(()));
    }

    #[test]
    fn nonces_are_uniform_digits_and_never_a_live_cards() {
        let ledger = CardLedger::default();
        // Bytes at or above 216 are skipped; the rest map to digit % 108.
        let mut bytes = [216u8, 255, 0, 107, 108, 215, 5, 6, 7].into_iter().cycle();
        let minted = ledger
            .mint(|pool| {
                pool.iter_mut()
                    .for_each(|byte| *byte = bytes.next().unwrap());
                true
            })
            .unwrap();
        assert_eq!(minted, InlineNonce::new([0, 107, 0, 107, 5, 6]).unwrap());
        let mut taken = CardLedger::default();
        taken.record(minted, card(1, 7), Instant::now());
        let same = [0u8, 107, 0, 107, 5, 6];
        assert_eq!(
            taken.mint(|pool| {
                for (index, byte) in pool.iter_mut().enumerate() {
                    *byte = same[index % same.len()];
                }
                true
            }),
            None,
            "every attempt collides with a live card"
        );
        assert_eq!(ledger.mint(|_| false), None, "no randomness, no nonce");
    }

    #[test]
    fn retiring_names_each_card_once_and_the_item_lookup_follows() {
        let mut ledger = CardLedger::default();
        let now = Instant::now();
        ledger.record(nonce(1), card(1, 7), now);
        ledger.record(nonce(2), card(2, 8), now);
        assert_eq!(ledger.item(1, nonce(1)), Some(1));
        assert_eq!(ledger.item(2, nonce(1)), None, "another pane's card");
        assert_eq!(ledger.owners(), vec![owner(7), owner(8)]);
        assert_eq!(
            ledger.retire(|card| card.owner == owner(7)),
            vec![(1, nonce(1))]
        );
        assert_eq!(ledger.item(1, nonce(1)), None);
        assert_eq!(ledger.len(), 1);
    }
}

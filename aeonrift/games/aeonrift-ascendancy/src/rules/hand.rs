//! The Clash-style hand: four playable cards plus a "next card" preview.
//!
//! A [`Hand`] draws from a seeded [`DrawPile`]: [`HAND_SIZE`] cards are visible and
//! playable at once, with one more previewed as "next". Playing a hand slot sends its
//! card to the back of the cycle (the preview slides into the played slot, and a fresh
//! card becomes the preview). The whole flow is deterministic given `(deck_len, seed)`.
//!
//! This is real, tested card-flow logic exposed for modes/clients that want the full
//! Clash experience. The default match runner intentionally deploys from the whole
//! deck instead (to keep its exact regression baselines stable), so `Hand` is layered
//! ON TOP of the rules core rather than baked into the core tick.

use super::deck::DrawPile;

/// Number of cards visible + playable at once.
pub const HAND_SIZE: usize = 4;

/// A rotating hand over a deck: `slots` are the currently playable deck indices, and
/// `next` is the previewed upcoming card.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hand {
    pile: DrawPile,
    slots: Vec<usize>,
    next: Option<usize>,
}

impl Hand {
    /// Deal the opening hand: up to [`HAND_SIZE`] cards plus one preview, from a
    /// seeded shuffle of `deck_len` deck slots.
    pub fn new(deck_len: usize, seed: u64) -> Self {
        let mut pile = DrawPile::new(deck_len, seed);
        let hand_n = HAND_SIZE.min(deck_len);
        let slots: Vec<usize> = (0..hand_n).filter_map(|_| pile.draw()).collect();
        let next = pile.draw();
        Hand { pile, slots, next }
    }

    /// The currently playable deck indices (hand slots).
    pub fn slots(&self) -> &[usize] {
        &self.slots
    }

    /// The previewed upcoming card's deck index, if any.
    pub fn peek_next(&self) -> Option<usize> {
        self.next
    }

    /// Play hand slot `slot`. Returns the deck index that was played and cycles the
    /// hand: the preview slides into the freed slot and a new card is previewed.
    /// `None` if `slot` is out of range.
    pub fn play(&mut self, slot: usize) -> Option<usize> {
        if slot >= self.slots.len() {
            return None;
        }
        let played = self.slots[slot];
        if let Some(n) = self.next.take() {
            self.slots[slot] = n;
            self.next = self.pile.draw();
        } else {
            // Only reachable for a deck smaller than the hand + preview.
            self.slots.remove(slot);
        }
        Some(played)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opening_hand_is_full_and_distinct_for_a_real_deck() {
        let h = Hand::new(8, 42);
        assert_eq!(h.slots().len(), HAND_SIZE);
        assert!(h.peek_next().is_some());
        let mut seen = h.slots().to_vec();
        seen.push(h.peek_next().unwrap());
        seen.sort_unstable();
        seen.dedup();
        assert_eq!(seen.len(), HAND_SIZE + 1, "opening 5 cards must be distinct");
    }

    #[test]
    fn playing_cycles_deterministically() {
        let mut a = Hand::new(8, 5);
        let mut b = Hand::new(8, 5);
        let mut plays_a = Vec::new();
        let mut plays_b = Vec::new();
        for _ in 0..24 {
            plays_a.push(a.play(0).unwrap());
            plays_b.push(b.play(0).unwrap());
        }
        assert_eq!(plays_a, plays_b, "same seed must cycle identically");
        // Over a full cycle every deck card is eventually played.
        let mut uniq = plays_a.clone();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq, (0..8).collect::<Vec<_>>());
    }

    #[test]
    fn out_of_range_slot_is_rejected() {
        let mut h = Hand::new(8, 1);
        assert_eq!(h.play(99), None);
    }
}

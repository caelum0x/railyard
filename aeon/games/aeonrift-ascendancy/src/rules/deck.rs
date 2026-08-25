//! The in-match draw pile: a deterministic, seeded cycle over a deck's cards.
//!
//! Clash-style card cycling: at match start the deck's 8 card slots are shuffled once
//! with a seeded [`sim::Rng`] (Fisher–Yates), then drawn in that fixed order, wrapping
//! forever. Because the permutation is a pure function of `(deck_len, seed)` the draw
//! sequence is identical on every machine and every run — the same determinism law as
//! the rest of the battler.
//!
//! This is the card-flow layer that sits UNDER [`super::hand`]. The default match
//! runner deploys straight from the full deck (so its regression baselines are stable);
//! [`Hand`](super::hand::Hand) is the richer Clash-style flow built on this pile.

use sim::Rng;

/// A shuffled, cycling order over `0..deck_len` deck slots.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DrawPile {
    order: Vec<usize>,
    cursor: usize,
}

impl DrawPile {
    /// Shuffle `deck_len` card slots deterministically from `seed`.
    pub fn new(deck_len: usize, seed: u64) -> Self {
        let mut order: Vec<usize> = (0..deck_len).collect();
        let mut rng = Rng::new(seed);
        // Fisher–Yates using the seeded RNG (integer modulo keeps it bit-exact).
        if deck_len > 1 {
            for i in (1..deck_len).rev() {
                let j = (rng.next_u64() % (i as u64 + 1)) as usize;
                order.swap(i, j);
            }
        }
        DrawPile { order, cursor: 0 }
    }

    /// Number of distinct card slots in the pile.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    /// True when the pile has no cards (empty deck).
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// The fixed shuffled order (a permutation of `0..deck_len`).
    pub fn order(&self) -> &[usize] {
        &self.order
    }

    /// Draw the next deck slot, advancing (and wrapping) the cursor. `None` only for
    /// an empty deck.
    pub fn draw(&mut self) -> Option<usize> {
        if self.order.is_empty() {
            return None;
        }
        let card = self.order[self.cursor];
        self.cursor = (self.cursor + 1) % self.order.len();
        Some(card)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shuffle_is_a_permutation() {
        let pile = DrawPile::new(8, 42);
        let mut seen = pile.order().to_vec();
        seen.sort_unstable();
        assert_eq!(seen, (0..8).collect::<Vec<_>>());
    }

    #[test]
    fn same_seed_same_order_different_seed_differs() {
        assert_eq!(DrawPile::new(8, 1).order(), DrawPile::new(8, 1).order());
        assert_ne!(DrawPile::new(8, 1).order(), DrawPile::new(8, 2).order());
    }

    #[test]
    fn draw_cycles_through_the_whole_deck_in_order() {
        let mut pile = DrawPile::new(8, 7);
        let expected = pile.order().to_vec();
        for round in 0..3 {
            for &e in &expected {
                assert_eq!(pile.draw(), Some(e), "cycle {round} diverged");
            }
        }
    }

    #[test]
    fn empty_deck_draws_nothing() {
        let mut pile = DrawPile::new(0, 1);
        assert!(pile.is_empty());
        assert_eq!(pile.draw(), None);
    }
}

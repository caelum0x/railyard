//! Draft mode: build a deck by picking, one card at a time.
//!
//! A [`Draft`] deals [`OPTIONS_PER_PICK`] card options per round (deterministically,
//! from a seeded [`sim::Rng`]) and the player picks one until a full deck of
//! [`STANDARD_DECK_SIZE`] cards is assembled. Options never repeat an already-drafted
//! card, so a draft always yields a legal (duplicate-free) deck. Indices are into a
//! [`CardPool`](crate::cards::CardPool)'s entries.

use sim::Rng;

use super::{Mode, ModeConfig, STANDARD_DECK_SIZE};
use crate::rules::config::MatchConfig;

/// Cards offered each pick.
pub const OPTIONS_PER_PICK: usize = 3;

/// The draft mode config (unranked, deck assembled on the spot).
pub fn config() -> ModeConfig {
    ModeConfig {
        mode: Mode::Draft,
        deck_size: STANDARD_DECK_SIZE,
        uses_preset_deck: false,
        ranked: false,
        bot: None,
        match_config: MatchConfig::default(),
    }
}

/// A running draft over a pool of `pool_size` cards.
#[derive(Clone, Debug)]
pub struct Draft {
    pool_size: usize,
    target: usize,
    rng: Rng,
    picks: Vec<usize>,
    current: Vec<usize>,
}

impl Draft {
    /// Start a draft for a full deck over `pool_size` cards, seeded by `seed`.
    pub fn new(pool_size: usize, seed: u64) -> Self {
        Self::with_target(pool_size, STANDARD_DECK_SIZE, seed)
    }

    /// Start a draft assembling `target` cards.
    pub fn with_target(pool_size: usize, target: usize, seed: u64) -> Self {
        let mut d = Draft {
            pool_size,
            target,
            rng: Rng::new(seed),
            picks: Vec::with_capacity(target),
            current: Vec::new(),
        };
        d.deal();
        d
    }

    /// The current pick's card options (pool indices).
    pub fn options(&self) -> &[usize] {
        &self.current
    }

    /// The cards drafted so far (pool indices, in pick order).
    pub fn picks(&self) -> &[usize] {
        &self.picks
    }

    /// Is the deck complete?
    pub fn is_complete(&self) -> bool {
        self.picks.len() >= self.target || self.current.is_empty()
    }

    /// Pick option `opt` (an index into [`options`](Self::options)). Records the chosen
    /// pool index, deals the next round, and returns the chosen pool index. `None` if
    /// `opt` is out of range or the draft is already complete.
    pub fn pick(&mut self, opt: usize) -> Option<usize> {
        if self.picks.len() >= self.target {
            return None;
        }
        let chosen = *self.current.get(opt)?;
        self.picks.push(chosen);
        if self.picks.len() < self.target {
            self.deal();
        } else {
            self.current.clear();
        }
        Some(chosen)
    }

    /// Deal a fresh set of options that excludes already-drafted cards.
    fn deal(&mut self) {
        self.current.clear();
        if self.pool_size == 0 {
            return;
        }
        let want = OPTIONS_PER_PICK.min(self.pool_size.saturating_sub(self.picks.len()));
        // Bounded rejection sampling: distinct, not already picked, not already offered.
        let mut guard = 0;
        while self.current.len() < want && guard < self.pool_size * 16 {
            guard += 1;
            let idx = (self.rng.next_u64() % self.pool_size as u64) as usize;
            if self.picks.contains(&idx) || self.current.contains(&idx) {
                continue;
            }
            self.current.push(idx);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draft_yields_a_full_distinct_deck() {
        let mut d = Draft::new(40, 42);
        while !d.is_complete() {
            d.pick(0).unwrap();
        }
        assert_eq!(d.picks().len(), STANDARD_DECK_SIZE);
        let mut uniq = d.picks().to_vec();
        uniq.sort_unstable();
        uniq.dedup();
        assert_eq!(uniq.len(), STANDARD_DECK_SIZE, "no duplicate picks");
    }

    #[test]
    fn draft_is_deterministic_per_seed() {
        let mut a = Draft::new(40, 7);
        let mut b = Draft::new(40, 7);
        assert_eq!(a.options(), b.options());
        while !a.is_complete() {
            let pa = a.pick(0);
            let pb = b.pick(0);
            assert_eq!(pa, pb);
            assert_eq!(a.options(), b.options());
        }
    }
}

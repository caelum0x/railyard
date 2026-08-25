//! The win condition: a base-destroyed outcome.
//!
//! The rule is Clash-simple: destroy the enemy base to win. If both bases fall on
//! the same tick it is a draw, and a match that reaches the tick cap with both bases
//! standing is also a draw (resolved by the runner). This module owns only the pure
//! mapping from base liveness to an [`Outcome`].

use super::match_state::{MatchState, Outcome};

/// Evaluate the outcome from the two bases' liveness. `None` while both stand.
pub fn evaluate(m: &MatchState) -> Option<Outcome> {
    match (m.base_alive(0), m.base_alive(1)) {
        (true, true) => None,
        (true, false) => Some(Outcome::Winner(0)),
        (false, true) => Some(Outcome::Winner(1)),
        (false, false) => Some(Outcome::Draw),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::config::MatchConfig;

    #[test]
    fn both_bases_standing_is_undecided() {
        let m = MatchState::new(MatchConfig::default(), vec![], vec![]);
        assert_eq!(evaluate(&m), None);
    }
}

//! The elixir economy: a pure, deterministic resource clock.
//!
//! Each player accrues one elixir every [`MatchConfig::elixir_regen_period`] ticks,
//! capped at [`MatchConfig::max_elixir`]. Regen is keyed on the world tick (not the
//! wall-clock), so the whole economy is a pure function of elapsed ticks. Deploy
//! affordability is checked here too, keeping the one economy rule in one place.

use super::match_state::MatchState;

/// Can `elixir` pay for a card costing `cost`?
pub fn can_afford(elixir: u32, cost: u32) -> bool {
    elixir >= cost
}

/// Add one elixir per player on each regen period (capped at the max), keyed on the
/// current world tick so regen is a pure, deterministic function of elapsed ticks.
pub fn regen_elixir(m: &mut MatchState) {
    let t = m.world.tick;
    let period = m.config.elixir_regen_period;
    if t > 0 && period > 0 && t.is_multiple_of(period) {
        for p in 0..2 {
            m.elixir[p] = (m.elixir[p] + 1).min(m.config.max_elixir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::config::MatchConfig;

    #[test]
    fn affordability_is_a_simple_threshold() {
        assert!(can_afford(5, 5));
        assert!(can_afford(6, 5));
        assert!(!can_afford(4, 5));
    }

    #[test]
    fn regen_only_fires_on_period_multiples_and_caps() {
        let cfg = MatchConfig::default();
        let mut m = MatchState::new(cfg, vec![], vec![]);
        m.world.tick = 1;
        regen_elixir(&mut m);
        assert_eq!(m.elixir, [cfg.start_elixir, cfg.start_elixir]);

        m.world.tick = cfg.elixir_regen_period;
        regen_elixir(&mut m);
        assert_eq!(m.elixir, [cfg.start_elixir + 1, cfg.start_elixir + 1]);

        for k in 2..30 {
            m.world.tick = cfg.elixir_regen_period * k;
            regen_elixir(&mut m);
        }
        assert_eq!(m.elixir, [cfg.max_elixir, cfg.max_elixir]);
    }
}

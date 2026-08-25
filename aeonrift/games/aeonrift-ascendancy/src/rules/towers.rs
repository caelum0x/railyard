//! Bases (towers): stationary combat bodies whose destruction ends the match.
//!
//! A base is just a `sim::Champion` that never receives a move input: it holds
//! position and auto-attacks any enemy that walks into its range, using the same
//! `sim` combat systems as every deployed unit. Its stats come from [`MatchConfig`],
//! so tuning a tower is a config change, not new combat code.

use sim::{Champion, EntityId};

use super::config::MatchConfig;
use super::lanes;

/// Build a base (tower) as a stationary `sim::Champion`.
pub fn build_base(id: EntityId, player: u8, cfg: &MatchConfig) -> Champion {
    Champion::new(id, player, lanes::base_pos(cfg, player))
        .with_max_health(cfg.base_hp)
        .with_attack_damage(cfg.base_ad)
        .with_armor(cfg.base_armor)
        .with_attack_range(cfg.base_range)
        .with_attack_period(cfg.base_attack_period)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_inherits_config_stats_and_is_full_hp() {
        let cfg = MatchConfig::default();
        let b = build_base(0, 0, &cfg);
        assert_eq!(b.health.max, cfg.base_hp);
        assert_eq!(b.health.current, cfg.base_hp);
        assert_eq!(b.attack_damage, cfg.base_ad);
        assert_eq!(b.armor, cfg.base_armor);
        assert_eq!(b.attack_range, cfg.base_range);
        assert_eq!(b.attack_period, cfg.base_attack_period);
        assert!(b.alive);
    }
}

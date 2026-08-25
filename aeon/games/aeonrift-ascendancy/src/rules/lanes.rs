//! Lane and base geometry.
//!
//! The battlefield is a fixed vertical strip: player 0's base sits at `-base_y`,
//! player 1's at `+base_y`, and units march along one of `lanes` evenly-spread
//! columns. All geometry is a pure function of [`MatchConfig`], so it is identical
//! on the server, in tests, and in the touch client (which reuses [`nearest_lane`]
//! to turn a tap into a lane).

use sim::Vec2;

use super::config::MatchConfig;

/// World-space position of a player's base (tower).
pub fn base_pos(cfg: &MatchConfig, player: u8) -> Vec2 {
    let y = if player == 0 { -cfg.base_y } else { cfg.base_y };
    Vec2::new(0.0, y)
}

/// The x-coordinate of `lane` (0-based). Lanes are spread evenly across
/// `[-lane_spread, lane_spread]`; a single lane sits at the center.
pub fn lane_x(cfg: &MatchConfig, lane: usize) -> f32 {
    if cfg.lanes <= 1 {
        return 0.0;
    }
    let t = lane as f32 / (cfg.lanes as f32 - 1.0); // 0..=1
    -cfg.lane_spread + t * (2.0 * cfg.lane_spread)
}

/// Spawn position for a unit deployed by `player` into `lane`: at the lane's x,
/// just in front of that player's base (offset toward mid-field).
pub fn spawn_pos(cfg: &MatchConfig, player: u8, lane: usize) -> Vec2 {
    let x = lane_x(cfg, lane);
    let y = if player == 0 {
        -cfg.base_y + cfg.spawn_offset
    } else {
        cfg.base_y - cfg.spawn_offset
    };
    Vec2::new(x, y)
}

/// The lane index whose center x is nearest to `x` (Clash-style: tap the field, the
/// unit joins the closest lane). Ties resolve to the lower index.
pub fn nearest_lane(cfg: &MatchConfig, x: f32) -> usize {
    let lanes = cfg.lanes.max(1);
    (0..lanes)
        .min_by(|&a, &b| {
            let da = (lane_x(cfg, a) - x).abs();
            let db = (lane_x(cfg, b) - x).abs();
            da.total_cmp(&db)
        })
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lanes_are_symmetric_and_ordered() {
        let cfg = MatchConfig::default();
        assert!(lane_x(&cfg, 0) < lane_x(&cfg, 1));
        assert!(lane_x(&cfg, 1) < lane_x(&cfg, 2));
        // Symmetric spread around 0 for the default 3-lane layout.
        assert!((lane_x(&cfg, 0) + lane_x(&cfg, 2)).abs() < 1e-4);
        assert!(lane_x(&cfg, 1).abs() < 1e-4);
    }

    #[test]
    fn nearest_lane_snaps_to_closest_column() {
        let cfg = MatchConfig::default();
        assert_eq!(nearest_lane(&cfg, lane_x(&cfg, 0) - 1.0), 0);
        assert_eq!(nearest_lane(&cfg, 0.1), 1);
        assert_eq!(nearest_lane(&cfg, lane_x(&cfg, 2) + 5.0), 2);
    }

    #[test]
    fn bases_face_each_other() {
        let cfg = MatchConfig::default();
        assert_eq!(base_pos(&cfg, 0).y, -cfg.base_y);
        assert_eq!(base_pos(&cfg, 1).y, cfg.base_y);
    }
}

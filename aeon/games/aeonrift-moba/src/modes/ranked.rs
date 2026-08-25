//! Ranked 5v5 on the standard arena. Same map/rules as Normal; the `ranked` flag
//! drives matchmaking (MMR, restrictions) which lives outside the sim.

use super::GameMode;

/// The Ranked queue config.
pub const fn config() -> GameMode {
    GameMode {
        name: "Ranked",
        map_name: "aeonrift_standard",
        team_size: 5,
        bots_on_red: false,
        ranked: true,
        starting_gold: 500,
    }
}

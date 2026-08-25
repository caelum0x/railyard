//! Normal 5v5 on the standard 3-lane arena (unranked, all human players).

use super::GameMode;

/// The Normal draft config.
pub const fn config() -> GameMode {
    GameMode {
        name: "Normal",
        map_name: "aeonrift_standard",
        team_size: 5,
        bots_on_red: false,
        ranked: false,
        starting_gold: 500,
    }
}

//! Co-op vs Bots: human BLUE team versus an AI-filled RED team on the standard
//! arena. The RED champions are driven by the deterministic laning bot
//! ([`crate::ai::laning_bot`]).

use super::GameMode;

/// The Co-op-vs-Bots config.
pub const fn config() -> GameMode {
    GameMode {
        name: "Co-op vs Bots",
        map_name: "aeonrift_standard",
        team_size: 5,
        bots_on_red: true,
        ranked: false,
        starting_gold: 500,
    }
}

//! Match modes: named configurations that pick a map, team size, bot fill, and
//! economy. A [`GameMode`] is plain data consumed by [`crate::game::Match::new`].

pub mod coop_vs_bots;
pub mod normal;
pub mod ranked;

/// A match-mode configuration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GameMode {
    /// Display name.
    pub name: &'static str,
    /// Map file stem under `content/maps/` (e.g. "aeonrift_standard").
    pub map_name: &'static str,
    /// Champions per team.
    pub team_size: usize,
    /// Fill the RED team with AI bots (coop-vs-bots / practice).
    pub bots_on_red: bool,
    /// Whether this is a ranked queue (affects matchmaking, not the sim).
    pub ranked: bool,
    /// Starting gold per team.
    pub starting_gold: i32,
}

impl GameMode {
    /// A single-lane practice config on an arbitrary map (used by tests + tools).
    pub const fn practice(map_name: &'static str, team_size: usize, bots_on_red: bool) -> Self {
        GameMode {
            name: "Practice",
            map_name,
            team_size,
            bots_on_red,
            ranked: false,
            starting_gold: 500,
        }
    }
}

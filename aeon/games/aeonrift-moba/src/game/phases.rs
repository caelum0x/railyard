//! Match lifecycle phases.
//!
//! A match moves Draft -> Loading -> InGame -> Ended. Only `InGame` ticks the sim;
//! `Ended` is terminal and records the winning team.

/// The lifecycle state of a match.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    /// Champion select / ban phase.
    Draft,
    /// Loading into the map (world built, not yet simulating).
    Loading,
    /// Live simulation.
    InGame,
    /// Terminal: a nexus fell; `winner` is the surviving team.
    Ended { winner: u8 },
}

impl Phase {
    /// Whether the match has ended.
    pub fn is_over(&self) -> bool {
        matches!(self, Phase::Ended { .. })
    }

    /// The winning team, if the match has ended.
    pub fn winner(&self) -> Option<u8> {
        match self {
            Phase::Ended { winner } => Some(*winner),
            _ => None,
        }
    }

    /// A small deterministic code for hashing/fingerprinting.
    pub fn code(&self) -> u64 {
        match self {
            Phase::Draft => 1,
            Phase::Loading => 2,
            Phase::InGame => 3,
            Phase::Ended { winner } => 100 + *winner as u64,
        }
    }
}

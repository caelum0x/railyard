//! Per-team gold + experience economy.
//!
//! Two player teams (0 = Blue, 1 = Red). Neutral-team awards are never stored here
//! (the killing player team receives the reward instead — see the match driver).

/// Gold + XP totals for the two player teams.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Economy {
    gold: [i32; 2],
    xp: [i32; 2],
}

impl Economy {
    /// A fresh economy with `starting` gold for both teams and 0 XP.
    pub fn new(starting: i32) -> Self {
        Economy {
            gold: [starting; 2],
            xp: [0; 2],
        }
    }

    fn idx(team: u8) -> Option<usize> {
        match team {
            0 => Some(0),
            1 => Some(1),
            _ => None,
        }
    }

    /// Current gold for a player team (0 for a non-player team).
    pub fn gold(&self, team: u8) -> i32 {
        Self::idx(team).map(|i| self.gold[i]).unwrap_or(0)
    }

    /// Current XP for a player team.
    pub fn xp(&self, team: u8) -> i32 {
        Self::idx(team).map(|i| self.xp[i]).unwrap_or(0)
    }

    /// Award gold + XP to a player team (a no-op for a non-player team).
    pub fn award(&mut self, team: u8, gold: i32, xp: i32) {
        if let Some(i) = Self::idx(team) {
            self.gold[i] += gold;
            self.xp[i] += xp;
        }
    }

    /// Try to spend `cost` gold from a team; returns false (no change) if too poor.
    pub fn spend(&mut self, team: u8, cost: i32) -> bool {
        match Self::idx(team) {
            Some(i) if self.gold[i] >= cost => {
                self.gold[i] -= cost;
                true
            }
            _ => false,
        }
    }

    /// Mutable handle to a team's gold slot (used by the shop).
    pub fn gold_mut(&mut self, team: u8) -> Option<&mut i32> {
        Self::idx(team).map(move |i| &mut self.gold[i])
    }
}

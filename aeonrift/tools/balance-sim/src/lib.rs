//! # aeonrift-balance-sim
//!
//! ROLE: RL self-play tournaments producing per-champion win-rate bands (the balance gate).
//!
//! MIGRATE: moon-lol crates/lol_rl (self-play) — new tournament driver.
//!
//! DEPENDS_ON (workspace path deps):
//!   - `aeonrift-ai-bots`
//!   - `aeonrift-combat`
//!   - `aeonrift-sim-core`
//!
//! STATUS: compiling stub. This is the consolidation skeleton described in
//! AEONRIFT-MASTER-PLAN.md; the real implementation migrates in from the source
//! above during the reconciliation step (see MIGRATION.md).

/// Placeholder marker type so the crate exposes a real, testable symbol.
/// Replaced by the migrated implementation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Placeholder;

impl Placeholder {
    /// Crate identity string — proves the stub links and runs.
    pub const CRATE: &'static str = "aeonrift-balance-sim";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_links() {
        assert_eq!(Placeholder::CRATE, "aeonrift-balance-sim");
        assert_eq!(Placeholder, Placeholder::default());
    }
}

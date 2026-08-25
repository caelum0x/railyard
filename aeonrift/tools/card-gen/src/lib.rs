//! # aeonrift-card-gen
//!
//! ROLE: Codegen: derive an Ascendancy card (cost/rarity) from a champion spec.
//!
//! MIGRATE: moba/cards (card_meta) — new codegen tool.
//!
//! DEPENDS_ON (workspace path deps):
//!   - `aeonrift-ability-dsl`
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
    pub const CRATE: &'static str = "aeonrift-card-gen";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_links() {
        assert_eq!(Placeholder::CRATE, "aeonrift-card-gen");
        assert_eq!(Placeholder, Placeholder::default());
    }
}

//! # aeonrift-champion-gen
//!
//! ROLE: Codegen: concept -> champion BUNDLE (content/<name>/ + engine plugin + card_meta).
//!
//! MIGRATE: moba/champions (spec authoring) — new codegen tool.
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
    pub const CRATE: &'static str = "aeonrift-champion-gen";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_links() {
        assert_eq!(Placeholder::CRATE, "aeonrift-champion-gen");
        assert_eq!(Placeholder, Placeholder::default());
    }
}

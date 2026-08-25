//! # aeonrift-asset-cli
//!
//! ROLE: Hash / upload / manifest CLI for the content-addressed asset store (no binaries in git).
//!
//! MIGRATE: moba/asset-pipeline (tools/aeonrift_assets: blake3, glb, manifest, roster).
//!
//! DEPENDS_ON (workspace path deps):
//!   - (none)
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
    pub const CRATE: &'static str = "aeonrift-asset-cli";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_links() {
        assert_eq!(Placeholder::CRATE, "aeonrift-asset-cli");
        assert_eq!(Placeholder, Placeholder::default());
    }
}

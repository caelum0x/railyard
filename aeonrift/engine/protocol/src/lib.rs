//! # aeonrift-protocol
//!
//! ROLE: Shared wire types: snapshots, inputs, commands (serde-stable across client/server).
//!
//! MIGRATE: moon-lol crates/lol_web_protocol + lol_share (wire schema).
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
    pub const CRATE: &'static str = "aeonrift-protocol";
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_links() {
        assert_eq!(Placeholder::CRATE, "aeonrift-protocol");
        assert_eq!(Placeholder, Placeholder::default());
    }
}

//! Stages 4-6 — extraction: parse the REAL repo (Cargo manifests + champion RON +
//! asset manifests) into typed nodes/edges. No LLM needed — the repo is already
//! structured; the extractors just lift that structure into the ontology.
//!
//! [`build`] runs all three extractors into one graph; because [`crate::graph::Graph`]
//! dedups on insert, Stage-8 fusion happens for free (one champion node even though
//! it `COMPILES_TO` two games and is referenced from Cargo + content + manifests).

pub mod assets;
pub mod cargo;
pub mod champions;

use std::path::Path;

use crate::graph::Graph;

/// Well-known singleton / product node ids shared across extractors.
pub mod ids {
    /// The two shipped games champions compile into.
    pub const GAME_MOBA: &str = "aeonrift-moba";
    pub const GAME_ASCENDANCY: &str = "aeonrift-ascendancy";
    /// The single content-addressed asset store (R2/CDN blob store).
    pub const ASSET_STORE: &str = "asset-store";
    /// The champion schema (closed-vocabulary validation gate).
    pub const CHAMPION_SCHEMA: &str = "champion.schema.ron";
    /// The deterministic roster duel/balance gate (engine/ability-dsl roster tests).
    pub const ROSTER_GATE: &str = "roster-duel";
}

/// Build the full repo graph by running every extractor over `repo_root`.
///
/// Errors from any single extractor are surfaced; a partial graph is never returned
/// silently. The result is deterministic for an unchanged repo.
pub fn build(repo_root: &Path) -> anyhow_lite::Result<Graph> {
    let mut graph = Graph::new();
    cargo::extract(repo_root, &mut graph)?;
    champions::extract(repo_root, &mut graph)?;
    assets::extract(repo_root, &mut graph)?;
    Ok(graph)
}

/// A tiny local error alias so the crate needs no `anyhow` dependency while still
/// giving extractors a uniform `Result` with context-carrying string errors.
pub mod anyhow_lite {
    /// Boxed, displayable extractor error.
    pub type Error = String;
    /// Uniform extractor result.
    pub type Result<T> = std::result::Result<T, Error>;

    /// Attach context to a `Result`/`Option`, mirroring `anyhow::Context`.
    pub trait Context<T> {
        fn context(self, msg: impl std::fmt::Display) -> Result<T>;
    }

    impl<T, E: std::fmt::Display> Context<T> for std::result::Result<T, E> {
        fn context(self, msg: impl std::fmt::Display) -> Result<T> {
            self.map_err(|e| format!("{msg}: {e}"))
        }
    }

    impl<T> Context<T> for Option<T> {
        fn context(self, msg: impl std::fmt::Display) -> Result<T> {
            self.ok_or_else(|| msg.to_string())
        }
    }
}

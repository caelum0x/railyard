//! # aeonrift-graph
//!
//! ROLE: AEONRIFT-GRAPH.md extractor + GraphRAG server. Parses the monorepo (Cargo
//! manifests + champion RON + asset manifests) into a typed knowledge graph and
//! serves impact-analysis / completeness / codegen-targeting queries over it.
//!
//! The 9-stage graph-engineering pipeline, applied to the codebase itself:
//!   - Stage 3 (ontology): [`ontology`] — 23 entity kinds, 17 relations with
//!     `domain()`/`range()` type signatures.
//!   - Stages 4-6 (extract): [`extract`] — Cargo / champions / assets extractors.
//!   - Stage 7 (quality gate): [`validate`] — reject edges violating domain/range.
//!   - Stage 8 (fusion): [`graph::Graph`] dedups nodes by `(kind, id)`.
//!   - Stage 9 (serve): [`query`] — deps/rdeps, champion subgraph, missing, stats, export;
//!     [`rag`] — GraphRAG serving (retrieve subgraph -> serialize -> grounded NL answer).
//!
//! MIGRATE: AEONRIFT-GRAPH.md (Stage 4-6 extraction) — implemented.

pub mod extract;
pub mod graph;
pub mod ontology;
pub mod query;
pub mod rag;
pub mod validate;

use std::path::{Path, PathBuf};

pub use graph::Graph;
pub use ontology::{Edge, EntityKind, Node, NodeRef, Provenance, RelationKind};

/// Build the full repo graph from `repo_root` (runs all extractors + fusion).
pub fn build(repo_root: &Path) -> extract::anyhow_lite::Result<Graph> {
    extract::build(repo_root)
}

/// Build the graph AND run the Stage-7 quality gate, returning both the graph and
/// any rejected edges (empty on a clean repo).
pub fn build_validated(
    repo_root: &Path,
) -> extract::anyhow_lite::Result<(Graph, validate::ValidationReport)> {
    let graph = build(repo_root)?;
    let report = validate::validate(&graph);
    Ok((graph, report))
}

/// Locate the workspace root: `AEONRIFT_REPO` env override, else walk up from the
/// current directory for a `Cargo.toml` containing `[workspace]`, else fall back to
/// the compile-time crate location (`tools/graph` -> repo root).
pub fn find_repo_root() -> PathBuf {
    if let Ok(p) = std::env::var("AEONRIFT_REPO") {
        return PathBuf::from(p);
    }
    if let Ok(cwd) = std::env::current_dir() {
        for anc in cwd.ancestors() {
            let manifest = anc.join("Cargo.toml");
            if manifest.is_file() {
                if let Ok(text) = std::fs::read_to_string(&manifest) {
                    if text.contains("[workspace]") {
                        return anc.to_path_buf();
                    }
                }
            }
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .unwrap_or_else(|_| PathBuf::from("."))
}

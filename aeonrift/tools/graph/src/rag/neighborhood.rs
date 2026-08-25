//! Bounded BFS retrieval over the extracted edges.
//!
//! Generalizes [`crate::query::champion_subgraph`] to any node and any traversal
//! direction, carrying full node/edge records (kinds + provenance) into a
//! [`Subgraph`]. Everything uses `BTreeSet`s, so the result is independent of
//! visitation order and byte-stable across runs.

use std::collections::BTreeSet;

use crate::graph::Graph;
use crate::ontology::{NodeRef, RelationKind};

use super::Subgraph;

/// Which arrows to follow during the walk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Follow outgoing edges (a node's forward bundle — the champion pattern).
    Out,
    /// Follow incoming edges (reverse — blast-radius / dependents).
    In,
    /// Follow both (use with a relation filter to avoid high-degree hub bleed).
    Both,
}

impl Direction {
    fn out(self) -> bool {
        matches!(self, Direction::Out | Direction::Both)
    }
    fn incoming(self) -> bool {
        matches!(self, Direction::In | Direction::Both)
    }
}

/// The k-hop neighborhood of a single seed in the given direction.
///
/// Forward (`Out`) from a `Champion` yields its own bundle without sibling bleed
/// (shared hubs like `Game`/`Schema`/`Gate` are sinks). Reverse (`In`) from a
/// `Crate` yields everything that (transitively) depends on it.
pub fn get_neighborhood(graph: &Graph, seed: &NodeRef, depth: usize, dir: Direction) -> Subgraph {
    neighborhood_multi(graph, std::slice::from_ref(seed), depth, dir, None)
}

/// Multi-seed bounded BFS with an optional relation whitelist.
///
/// `rel_filter = None` follows every relation; `Some(set)` restricts the walk to
/// those relations (used for impact queries: reverse `DEPENDS_ON` + `COMPILES_TO`).
pub(crate) fn neighborhood_multi(
    graph: &Graph,
    seeds: &[NodeRef],
    depth: usize,
    dir: Direction,
    rel_filter: Option<&BTreeSet<RelationKind>>,
) -> Subgraph {
    let mut visited: BTreeSet<NodeRef> = BTreeSet::new();
    let mut frontier: BTreeSet<NodeRef> = BTreeSet::new();
    for s in seeds {
        if graph.contains(s) {
            visited.insert(s.clone());
            frontier.insert(s.clone());
        }
    }

    let mut collected: BTreeSet<(NodeRef, RelationKind, NodeRef)> = BTreeSet::new();

    for _ in 0..depth {
        let mut next: BTreeSet<NodeRef> = BTreeSet::new();
        for src in &frontier {
            if dir.out() {
                for e in graph.out_edges(src) {
                    if rel_ok(rel_filter, e.rel) {
                        collected.insert(e.key());
                        if !visited.contains(&e.to) {
                            next.insert(e.to.clone());
                        }
                    }
                }
            }
            if dir.incoming() {
                for e in graph.in_edges(src) {
                    if rel_ok(rel_filter, e.rel) {
                        collected.insert(e.key());
                        if !visited.contains(&e.from) {
                            next.insert(e.from.clone());
                        }
                    }
                }
            }
        }
        for n in &next {
            visited.insert(n.clone());
        }
        if next.is_empty() {
            break;
        }
        frontier = next;
    }

    Subgraph::build(graph, seeds.to_vec(), &visited, &collected)
}

fn rel_ok(filter: Option<&BTreeSet<RelationKind>>, rel: RelationKind) -> bool {
    filter.is_none_or(|set| set.contains(&rel))
}

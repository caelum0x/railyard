//! The in-memory property graph plus Stage-8 fusion.
//!
//! Nodes are keyed by their `(kind, id)` handle and edges by `(from, rel, to)`, both
//! in `BTreeMap`s — so adding the same fact twice is a no-op (fusion) and iteration
//! is deterministic. This is where the "one champion node across both games" invariant
//! lives: every `add_node` for the same `(kind, id)` merges into the single node.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::ontology::{Edge, EntityKind, Node, NodeRef, RelationKind};

/// A typed knowledge graph: deduplicated nodes + edges, deterministically ordered.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Graph {
    nodes: BTreeMap<NodeRef, Node>,
    edges: BTreeMap<(NodeRef, RelationKind, NodeRef), Edge>,
}

impl Graph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Stage-8 fusion: insert a node, deduping by `(kind, id)`. If a node with the
    /// same handle already exists, its props are merged (existing values win on
    /// conflict, so the first authoritative source is stable) and the new node's
    /// props fill any gaps. Provenance of the first insert is retained.
    pub fn add_node(&mut self, node: Node) {
        let key = node.node_ref();
        match self.nodes.get_mut(&key) {
            Some(existing) => {
                for (k, v) in node.props {
                    existing.props.entry(k).or_insert(v);
                }
            }
            None => {
                self.nodes.insert(key, node);
            }
        }
    }

    /// Insert an edge, deduping by `(from, rel, to)`.
    pub fn add_edge(&mut self, edge: Edge) {
        self.edges.entry(edge.key()).or_insert(edge);
    }

    /// All nodes, in deterministic `(kind, id)` order.
    pub fn nodes(&self) -> impl Iterator<Item = &Node> {
        self.nodes.values()
    }

    /// All edges, in deterministic `(from, rel, to)` order.
    pub fn edges(&self) -> impl Iterator<Item = &Edge> {
        self.edges.values()
    }

    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    pub fn edge_count(&self) -> usize {
        self.edges.len()
    }

    /// Look up a node by handle.
    pub fn get(&self, r: &NodeRef) -> Option<&Node> {
        self.nodes.get(r)
    }

    pub fn contains(&self, r: &NodeRef) -> bool {
        self.nodes.contains_key(r)
    }

    /// All nodes of a given kind.
    pub fn nodes_of(&self, kind: EntityKind) -> impl Iterator<Item = &Node> {
        self.nodes.values().filter(move |n| n.kind == kind)
    }

    /// All edges of a given relation kind.
    pub fn edges_of(&self, rel: RelationKind) -> impl Iterator<Item = &Edge> {
        self.edges.values().filter(move |e| e.rel == rel)
    }

    /// Node counts grouped by kind (deterministic order).
    pub fn node_counts_by_kind(&self) -> BTreeMap<EntityKind, usize> {
        let mut out: BTreeMap<EntityKind, usize> = BTreeMap::new();
        for n in self.nodes.values() {
            *out.entry(n.kind).or_insert(0) += 1;
        }
        out
    }

    /// Edge counts grouped by relation kind (deterministic order).
    pub fn edge_counts_by_kind(&self) -> BTreeMap<RelationKind, usize> {
        let mut out: BTreeMap<RelationKind, usize> = BTreeMap::new();
        for e in self.edges.values() {
            *out.entry(e.rel).or_insert(0) += 1;
        }
        out
    }

    /// Outgoing edges from a node handle.
    pub fn out_edges<'a>(&'a self, from: &'a NodeRef) -> impl Iterator<Item = &'a Edge> + 'a {
        self.edges.values().filter(move |e| &e.from == from)
    }

    /// Incoming edges to a node handle.
    pub fn in_edges<'a>(&'a self, to: &'a NodeRef) -> impl Iterator<Item = &'a Edge> + 'a {
        self.edges.values().filter(move |e| &e.to == to)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ontology::Provenance;

    fn champ(id: &str) -> Node {
        Node::new(EntityKind::Champion, id, Provenance::from("x.ron", "human"))
    }

    #[test]
    fn fusion_dedups_nodes_by_kind_and_id() {
        let mut g = Graph::new();
        g.add_node(champ("erebos").with_prop("name", "Erebos"));
        // Same champion added again (e.g. seen while compiling to a second game).
        g.add_node(champ("erebos").with_prop("role", "assassin"));
        assert_eq!(g.node_count(), 1, "one champion node, not two");
        let n = g
            .get(&NodeRef::new(EntityKind::Champion, "erebos"))
            .unwrap();
        // Merged props from both inserts.
        assert_eq!(n.props.get("name").map(String::as_str), Some("Erebos"));
        assert_eq!(n.props.get("role").map(String::as_str), Some("assassin"));
    }

    #[test]
    fn same_id_different_kind_are_distinct_nodes() {
        let mut g = Graph::new();
        g.add_node(champ("achilles"));
        g.add_node(Node::new(
            EntityKind::AssetManifest,
            "achilles",
            Provenance::from("m.ron", "blender"),
        ));
        assert_eq!(g.node_count(), 2);
    }

    #[test]
    fn edges_dedup_on_from_rel_to() {
        let mut g = Graph::new();
        let e = Edge::new(
            NodeRef::new(EntityKind::Crate, "a"),
            RelationKind::DependsOn,
            NodeRef::new(EntityKind::Crate, "b"),
            Provenance::from("Cargo.toml", "cargo-parse"),
        );
        g.add_edge(e.clone());
        g.add_edge(e);
        assert_eq!(g.edge_count(), 1);
    }
}

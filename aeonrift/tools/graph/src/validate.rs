//! Stage 7 — the quality gate: domain/range type-checking of every edge.
//!
//! This is `content-lint` generalized from a single champion to the whole
//! architecture. An edge survives only if BOTH endpoints exist as nodes AND the
//! endpoint kinds satisfy the relation's `domain()`/`range()`. A mis-wired build
//! dependency or a hallucinated `Ability -USES_EFFECT-> Champion` edge is rejected
//! here, exactly the way a knowledge graph rejects type-invalid structure.

use crate::graph::Graph;
use crate::ontology::Edge;

/// Why an edge was rejected by the gate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rejection {
    /// Endpoint kinds violate the relation's domain/range signature.
    TypeMismatch,
    /// The `from` node does not exist in the graph.
    MissingFrom,
    /// The `to` node does not exist in the graph.
    MissingTo,
}

/// A rejected edge plus the reason it failed the gate.
#[derive(Debug, Clone)]
pub struct RejectedEdge {
    pub edge: Edge,
    pub reason: Rejection,
}

impl RejectedEdge {
    /// One-line human-readable explanation.
    pub fn describe(&self) -> String {
        let e = &self.edge;
        let reason = match self.reason {
            Rejection::TypeMismatch => format!(
                "{:?}:{} -{}-> {:?}:{} violates {} :: domain {:?} -> range {:?}",
                e.from.kind,
                e.from.id,
                e.rel.verb(),
                e.to.kind,
                e.to.id,
                e.rel.verb(),
                e.rel.domain(),
                e.rel.range(),
            ),
            Rejection::MissingFrom => {
                format!(
                    "{:?}:{} (from) does not exist as a node",
                    e.from.kind, e.from.id
                )
            }
            Rejection::MissingTo => {
                format!("{:?}:{} (to) does not exist as a node", e.to.kind, e.to.id)
            }
        };
        format!("REJECT {}: {}", e.rel.verb(), reason)
    }
}

/// The result of running the gate over a candidate graph.
#[derive(Debug, Clone, Default)]
pub struct ValidationReport {
    pub rejected: Vec<RejectedEdge>,
}

impl ValidationReport {
    pub fn is_clean(&self) -> bool {
        self.rejected.is_empty()
    }
}

/// Run the domain/range gate over every edge in `graph`, reporting violations.
/// A clean repo produces an empty report.
pub fn validate(graph: &Graph) -> ValidationReport {
    let mut rejected = Vec::new();
    for edge in graph.edges() {
        // Endpoints must exist (fusion should guarantee this, but the gate proves it).
        if !graph.contains(&edge.from) {
            rejected.push(RejectedEdge {
                edge: edge.clone(),
                reason: Rejection::MissingFrom,
            });
            continue;
        }
        if !graph.contains(&edge.to) {
            rejected.push(RejectedEdge {
                edge: edge.clone(),
                reason: Rejection::MissingTo,
            });
            continue;
        }
        if !edge.rel.accepts(edge.from.kind, edge.to.kind) {
            rejected.push(RejectedEdge {
                edge: edge.clone(),
                reason: Rejection::TypeMismatch,
            });
        }
    }
    ValidationReport { rejected }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ontology::{EntityKind, Node, NodeRef, Provenance, RelationKind};

    #[test]
    fn clean_graph_passes_and_bad_edge_is_rejected() {
        let mut g = Graph::new();
        let ability = Node::new(
            EntityKind::Ability,
            "erebos:Shadowlance",
            Provenance::from("erebos.ron", "ability-dsl-loader"),
        );
        let effect = Node::new(
            EntityKind::EffectType,
            "Damage",
            Provenance::from("erebos.ron", "ability-dsl-loader"),
        );
        let champ = Node::new(
            EntityKind::Champion,
            "erebos",
            Provenance::from("erebos.ron", "human"),
        );
        g.add_node(ability.clone());
        g.add_node(effect.clone());
        g.add_node(champ.clone());

        // Valid: Ability -USES_EFFECT-> EffectType.
        g.add_edge(Edge::new(
            ability.node_ref(),
            RelationKind::UsesEffect,
            effect.node_ref(),
            Provenance::from("erebos.ron", "ability-dsl-loader"),
        ));
        assert!(validate(&g).is_clean());

        // Inject a synthetic bad edge: Champion -USES_EFFECT-> EffectType. The domain
        // of USES_EFFECT is [Ability] only, so the gate must reject it.
        g.add_edge(Edge::new(
            champ.node_ref(),
            RelationKind::UsesEffect,
            effect.node_ref(),
            Provenance::from("synthetic", "test"),
        ));
        let report = validate(&g);
        assert_eq!(report.rejected.len(), 1);
        assert_eq!(report.rejected[0].reason, Rejection::TypeMismatch);
        assert_eq!(report.rejected[0].edge.from, champ.node_ref());
    }

    #[test]
    fn dangling_endpoint_is_rejected() {
        let mut g = Graph::new();
        let a = NodeRef::new(EntityKind::Crate, "a");
        g.add_node(Node::new(
            EntityKind::Crate,
            "a",
            Provenance::from("Cargo.toml", "cargo-parse"),
        ));
        // `b` never added.
        g.add_edge(Edge::new(
            a,
            RelationKind::DependsOn,
            NodeRef::new(EntityKind::Crate, "b"),
            Provenance::from("Cargo.toml", "cargo-parse"),
        ));
        let report = validate(&g);
        assert_eq!(report.rejected.len(), 1);
        assert_eq!(report.rejected[0].reason, Rejection::MissingTo);
    }
}

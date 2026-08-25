//! Deterministic, LLM-ready serialization of a [`Subgraph`].
//!
//! Nodes are grouped by kind (declaration order via the derived `Ord`), edges are
//! rendered as `<from> --REL--> <to> (provenance)`, and a provenance footer lists
//! every distinct source. Because the subgraph is pre-sorted and this function
//! only appends in sorted order, the same subgraph always yields byte-identical
//! text — the property the grounding layer and the determinism test rely on.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::ontology::{EntityKind, Provenance};

use super::{edge_key, node_key, Subgraph};

/// Render `sub` as a stable, LLM-consumable string.
pub fn serialize_for_llm(sub: &Subgraph) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# SUBGRAPH ({} nodes, {} edges)",
        sub.nodes.len(),
        sub.edges.len()
    );

    if !sub.seeds.is_empty() {
        let mut seeds: Vec<String> = sub.seeds.iter().map(node_key).collect();
        seeds.sort();
        seeds.dedup();
        let _ = writeln!(out, "seeds: {}", seeds.join(", "));
    }

    out.push_str("\n## NODES\n");
    let mut current: Option<EntityKind> = None;
    for n in &sub.nodes {
        if current != Some(n.kind) {
            let _ = writeln!(out, "### {:?}", n.kind);
            current = Some(n.kind);
        }
        let props = if n.props.is_empty() {
            String::new()
        } else {
            let pairs: Vec<String> = n
                .props
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            format!("  {{{}}}", pairs.join(", "))
        };
        let _ = writeln!(out, "- {}{}", node_key(&n.node_ref()), props);
    }

    out.push_str("\n## EDGES\n");
    for e in &sub.edges {
        let _ = writeln!(out, "- {}  ({})", edge_key(e), provenance_str(&e.provenance));
    }

    out.push_str("\n## PROVENANCE\n");
    let mut provs: BTreeSet<String> = BTreeSet::new();
    for n in &sub.nodes {
        provs.insert(provenance_str(&n.provenance));
    }
    for e in &sub.edges {
        provs.insert(provenance_str(&e.provenance));
    }
    for p in provs {
        let _ = writeln!(out, "- {p}");
    }

    out
}

fn provenance_str(p: &Provenance) -> String {
    match &p.generator {
        Some(g) => format!("{} [{}]", p.source, g),
        None => p.source.clone(),
    }
}

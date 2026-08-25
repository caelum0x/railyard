//! Stage 9 — serve: GraphRAG-style queries over the built graph.
//!
//! Everything here is a read-only view producing deterministically ordered results:
//!   - [`deps`] / [`rdeps`] — forward / reverse `DEPENDS_ON` closure (blast radius).
//!   - [`champion_subgraph`] — a champion's k-hop neighborhood (the GraphRAG unit).
//!   - [`missing`] — the completeness critic (no model / faction / test).
//!   - [`stats`], [`export`] — counts and whole-graph serialization.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::graph::Graph;
use crate::ontology::{Edge, EntityKind, NodeRef, RelationKind};

/// Resolve a user-supplied crate name (`sim-core`, `aeonrift-sim-core`, or the
/// directory basename) to its canonical `Crate` node id, if it exists.
pub fn resolve_crate(graph: &Graph, input: &str) -> Option<String> {
    for n in graph.nodes_of(EntityKind::Crate) {
        let dir = n.props.get("dir").map(String::as_str);
        let short = n.props.get("short").map(String::as_str);
        if n.id == input
            || format!("aeonrift-{input}") == n.id
            || dir == Some(input)
            || short == Some(input)
        {
            return Some(n.id.clone());
        }
    }
    None
}

/// Forward `DEPENDS_ON` transitive closure: everything `crate_id` (transitively)
/// depends on. Deterministically sorted, excludes the crate itself.
pub fn deps(graph: &Graph, crate_id: &str) -> Vec<String> {
    closure(graph, crate_id, Direction::Forward)
}

/// Reverse `DEPENDS_ON` transitive closure: everything that (transitively) depends
/// on `crate_id` — i.e. the blast radius of changing it.
pub fn rdeps(graph: &Graph, crate_id: &str) -> Vec<String> {
    closure(graph, crate_id, Direction::Reverse)
}

enum Direction {
    Forward,
    Reverse,
}

fn closure(graph: &Graph, crate_id: &str, dir: Direction) -> Vec<String> {
    let start = NodeRef::new(EntityKind::Crate, crate_id);
    let mut seen: BTreeSet<String> = BTreeSet::new();
    let mut queue: VecDeque<NodeRef> = VecDeque::new();
    queue.push_back(start.clone());
    while let Some(cur) = queue.pop_front() {
        let next: Vec<NodeRef> = match dir {
            Direction::Forward => graph
                .out_edges(&cur)
                .filter(|e| e.rel == RelationKind::DependsOn)
                .map(|e| e.to.clone())
                .collect(),
            Direction::Reverse => graph
                .in_edges(&cur)
                .filter(|e| e.rel == RelationKind::DependsOn)
                .map(|e| e.from.clone())
                .collect(),
        };
        for n in next {
            if n.id != crate_id && seen.insert(n.id.clone()) {
                queue.push_back(n);
            }
        }
    }
    seen.into_iter().collect()
}

/// The k-hop subgraph around a champion: abilities, effects, faction, the games it
/// compiles to, its gates, and its assets. This is the GraphRAG retrieval unit for
/// "everything about champion X".
///
/// The traversal is **forward-directed** (follows outgoing edges only). That is
/// deliberate: shared singleton hubs (a `Game`, the `Schema`, the roster `Gate`)
/// are high-degree, so an undirected walk would bleed out into every sibling
/// champion. Following the arrows down keeps the neighborhood to this champion's own
/// bundle: `Champion -> Ability -> EffectType` and
/// `Champion -> AssetManifest -> Asset -> {AssetStore, Tool}`.
pub fn champion_subgraph(graph: &Graph, slug: &str, hops: usize) -> Vec<Edge> {
    let start = NodeRef::new(EntityKind::Champion, slug);
    if !graph.contains(&start) {
        return Vec::new();
    }
    let mut frontier: BTreeSet<NodeRef> = BTreeSet::new();
    frontier.insert(start);
    let mut visited: BTreeSet<NodeRef> = frontier.clone();
    let mut collected: BTreeSet<(NodeRef, RelationKind, NodeRef)> = BTreeSet::new();

    for _ in 0..hops {
        let mut next: BTreeSet<NodeRef> = BTreeSet::new();
        for src in &frontier {
            for edge in graph.out_edges(src) {
                collected.insert(edge.key());
                if !visited.contains(&edge.to) {
                    next.insert(edge.to.clone());
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

    // Rebuild full edges (with provenance) from the collected keys, in order.
    graph
        .edges()
        .filter(|e| collected.contains(&e.key()))
        .cloned()
        .collect()
}

/// One champion's completeness gaps (empty vec == complete).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Incomplete {
    pub slug: String,
    /// Human-readable missing pieces: "model", "faction", "test".
    pub missing: Vec<String>,
}

/// Completeness critic: champions lacking a `REFERENCES_ASSET` (no 3D model), a
/// `BELONGS_TO` (no faction), or a `TESTED_BY` (untested). Deterministically ordered.
pub fn missing(graph: &Graph) -> Vec<Incomplete> {
    let mut out = Vec::new();
    for champ in graph.nodes_of(EntityKind::Champion) {
        let cref = champ.node_ref();
        let mut gaps = Vec::new();
        if !graph
            .out_edges(&cref)
            .any(|e| e.rel == RelationKind::ReferencesAsset)
        {
            gaps.push("model".to_string());
        }
        if !graph
            .out_edges(&cref)
            .any(|e| e.rel == RelationKind::BelongsTo)
        {
            gaps.push("faction".to_string());
        }
        if !graph
            .out_edges(&cref)
            .any(|e| e.rel == RelationKind::TestedBy)
        {
            gaps.push("test".to_string());
        }
        if !gaps.is_empty() {
            out.push(Incomplete {
                slug: champ.id.clone(),
                missing: gaps,
            });
        }
    }
    out
}

/// Node + edge counts per kind, for the `stats` command.
pub struct Stats {
    pub nodes: BTreeMap<EntityKind, usize>,
    pub edges: BTreeMap<RelationKind, usize>,
    pub node_total: usize,
    pub edge_total: usize,
}

pub fn stats(graph: &Graph) -> Stats {
    Stats {
        nodes: graph.node_counts_by_kind(),
        edges: graph.edge_counts_by_kind(),
        node_total: graph.node_count(),
        edge_total: graph.edge_count(),
    }
}

/// Export formats for the whole graph.
#[derive(Debug, Clone, Copy)]
pub enum ExportFormat {
    Json,
    Mermaid,
    Dot,
}

impl ExportFormat {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "json" => Some(Self::Json),
            "mermaid" => Some(Self::Mermaid),
            "dot" => Some(Self::Dot),
            _ => None,
        }
    }
}

/// Serialize the whole graph deterministically in the requested format.
pub fn export(graph: &Graph, format: ExportFormat) -> String {
    match format {
        ExportFormat::Json => export_json(graph),
        ExportFormat::Mermaid => export_mermaid(graph),
        ExportFormat::Dot => export_dot(graph),
    }
}

fn export_json(graph: &Graph) -> String {
    // Graph derives Serialize; nodes/edges are already in BTreeMaps -> stable.
    serde_json::to_string_pretty(graph).unwrap_or_else(|e| format!("{{\"error\":\"{e}\"}}"))
}

fn export_mermaid(graph: &Graph) -> String {
    let mut out = String::from("graph LR\n");
    for e in graph.edges() {
        out.push_str(&format!(
            "  {} -->|{}| {}\n",
            mermaid_id(&e.from),
            e.rel.verb(),
            mermaid_id(&e.to),
        ));
    }
    out
}

fn export_dot(graph: &Graph) -> String {
    let mut out = String::from("digraph aeonrift {\n  rankdir=LR;\n");
    for n in graph.nodes() {
        out.push_str(&format!(
            "  \"{}\" [label=\"{}\\n({:?})\"];\n",
            node_key(&n.node_ref()),
            n.id.replace('"', "'"),
            n.kind,
        ));
    }
    for e in graph.edges() {
        out.push_str(&format!(
            "  \"{}\" -> \"{}\" [label=\"{}\"];\n",
            node_key(&e.from),
            node_key(&e.to),
            e.rel.verb(),
        ));
    }
    out.push_str("}\n");
    out
}

fn node_key(r: &NodeRef) -> String {
    format!("{:?}:{}", r.kind, r.id)
}

fn mermaid_id(r: &NodeRef) -> String {
    // Mermaid node ids cannot contain arbitrary punctuation; sanitize + label.
    let safe: String = format!("{:?}_{}", r.kind, r.id)
        .chars()
        .map(|c| if c.is_alphanumeric() { c } else { '_' })
        .collect();
    format!(
        "{safe}[\"{}:{}\"]",
        kind_short(r.kind),
        r.id.replace('"', "'")
    )
}

fn kind_short(kind: EntityKind) -> String {
    format!("{kind:?}")
}

//! Stage 9 (serve) — GraphRAG over the repo knowledge graph.
//!
//! This is the same three-move discipline the domain GraphRAG uses, applied to
//! the *repo* graph:
//!   1. **retrieve** a bounded subgraph around a seed ([`retrieve`], [`get_neighborhood`]),
//!   2. **serialize** it deterministically for an LLM ([`serialize_for_llm`]),
//!   3. **answer** a natural-language question, grounded strictly in that
//!      subgraph ([`answer`]).
//!
//! The LLM is reached through an INJECTABLE [`Ask`] seam: tests pass a mock, the
//! default build needs no API key, and a real Anthropic client is wired in only
//! behind the `llm` feature ([`llm::DefaultAsk`]). GROUNDING is enforced in
//! [`answer`]: any citation the model returns that is not actually a node/edge of
//! the retrieved subgraph is dropped — a fabricated reference can never survive.

pub mod neighborhood;
pub mod serialize;

#[cfg(feature = "llm")]
pub mod llm;

use std::collections::BTreeSet;

use crate::graph::Graph;
use crate::ontology::{Edge, EntityKind, Node, NodeRef, RelationKind};
use crate::query;

pub use neighborhood::{get_neighborhood, Direction};
pub use serialize::serialize_for_llm;

/// A bounded, deterministically-ordered slice of the graph: the GraphRAG
/// retrieval unit. Nodes are sorted by `(kind, id)`, edges by `(from, rel, to)`,
/// so the same subgraph always serializes byte-identically regardless of how it
/// was built.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Subgraph {
    /// The seed node(s) the neighborhood grew from (for the serialized header).
    pub seeds: Vec<NodeRef>,
    /// Full node records (props + provenance), sorted by `(kind, id)`.
    pub nodes: Vec<Node>,
    /// Full edge records (provenance carried), in `(from, rel, to)` order.
    pub edges: Vec<Edge>,
}

impl Subgraph {
    /// An empty subgraph (no seed resolved).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Build from a visited-node set + collected-edge-key set, pulling full
    /// records from `graph` in deterministic order.
    pub(crate) fn build(
        graph: &Graph,
        seeds: Vec<NodeRef>,
        visited: &BTreeSet<NodeRef>,
        edge_keys: &BTreeSet<(NodeRef, RelationKind, NodeRef)>,
    ) -> Self {
        let mut nodes: Vec<Node> = visited.iter().filter_map(|r| graph.get(r).cloned()).collect();
        nodes.sort_by(|a, b| (a.kind, a.id.as_str()).cmp(&(b.kind, b.id.as_str())));
        // graph.edges() is already in (from, rel, to) BTreeMap order — keep it.
        let edges: Vec<Edge> = graph
            .edges()
            .filter(|e| edge_keys.contains(&e.key()))
            .cloned()
            .collect();
        Self {
            seeds,
            nodes,
            edges,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.edges.is_empty()
    }

    /// Is `r` one of the nodes in this subgraph?
    pub fn contains_node(&self, r: &NodeRef) -> bool {
        self.nodes.iter().any(|n| &n.node_ref() == r)
    }

    /// Does any edge in the subgraph use relation `rel`?
    pub fn has_rel(&self, rel: RelationKind) -> bool {
        self.edges.iter().any(|e| e.rel == rel)
    }

    /// The `(kind, id)` handles of every node, deterministically ordered.
    pub fn node_refs(&self) -> Vec<NodeRef> {
        self.nodes.iter().map(Node::node_ref).collect()
    }

    /// Stable string keys ("Kind:id") for every node — the citation vocabulary.
    pub fn node_keys(&self) -> BTreeSet<String> {
        self.nodes.iter().map(|n| node_key(&n.node_ref())).collect()
    }

    /// Stable string keys for every edge — also a valid citation vocabulary.
    pub fn edge_keys(&self) -> BTreeSet<String> {
        self.edges.iter().map(edge_key).collect()
    }
}

/// A node's stable citation key, e.g. `Champion:achilles`. Matches the identifier
/// the serializer prints, so a model can cite exactly what it was shown.
pub fn node_key(r: &NodeRef) -> String {
    format!("{:?}:{}", r.kind, r.id)
}

/// An edge's stable citation key, e.g.
/// `Champion:achilles --HAS_ABILITY--> Ability:achilles:Spear`.
pub fn edge_key(e: &Edge) -> String {
    format!(
        "{:?}:{} --{}--> {:?}:{}",
        e.from.kind,
        e.from.id,
        e.rel.verb(),
        e.to.kind,
        e.to.id,
    )
}

/// The result of a grounded NL query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RagAnswer {
    /// The model's prose answer (or an error/notice when no LLM is available).
    pub answer: String,
    /// Node/edge citation keys — GROUNDED: every entry provably exists in
    /// [`RagAnswer::subgraph`]; fabricated references are dropped.
    pub citations: Vec<String>,
    /// The nodes actually cited, resolved back to handles (subset of the subgraph).
    pub used_nodes: Vec<NodeRef>,
    /// The subgraph the answer was grounded in (retrieved + serialized).
    pub subgraph: Subgraph,
}

/// The injectable LLM seam. Implement this to plug in any client; tests use a
/// mock, [`llm::DefaultAsk`] uses Anthropic behind the `llm` feature.
pub trait Ask {
    /// Produce an answer + proposed citations from a system prompt, the
    /// serialized subgraph `context`, and the user `question`.
    fn ask(&self, system: &str, context: &str, question: &str) -> Result<LlmResponse, String>;
}

/// A raw LLM response before grounding. `citations` are *proposed* — [`answer`]
/// filters them against the retrieved subgraph.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LlmResponse {
    pub answer: String,
    pub citations: Vec<String>,
}

/// The system prompt: answer only from the provided subgraph, cite ids verbatim.
pub const SYSTEM_PROMPT: &str = "\
You are a code-architecture analyst answering questions about the AEONRIFT monorepo \
from a knowledge graph. You are given a SUBGRAPH: a set of typed NODES and directed \
EDGES extracted from the repo (Cargo manifests, champion specs, asset manifests).\n\
Rules:\n\
- Answer ONLY from the SUBGRAPH. Never invent nodes, edges, crates, or champions \
that are not present.\n\
- Ground every claim in specific node/edge ids from the SUBGRAPH.\n\
- Reply as a JSON object: {\"answer\": <concise prose>, \"citations\": [<node or edge ids>]}\n\
- Each citation MUST be copied verbatim from a NODES line (e.g. \"Crate:aeonrift-combat\") \
or an EDGES line (e.g. \"Champion:achilles --HAS_ABILITY--> Ability:achilles:Spear\").";

/// Resolve a natural-language question to a retrieved subgraph.
///
/// Intent routing mirrors the domain GraphRAG's seed-selection:
///   - "missing / incomplete" -> the completeness pass (incomplete champions).
///   - "break / impact / change <crate>" -> seed the crate, expand REVERSE
///     `DEPENDS_ON` + `COMPILES_TO` (the blast radius).
///   - "<champion> touches / uses ..." -> the champion's forward bundle.
///   - a bare crate mention -> that crate's forward `DEPENDS_ON` closure.
///   - otherwise -> an empty subgraph (nothing to ground on).
pub fn retrieve(graph: &Graph, question: &str) -> Subgraph {
    let q = question.to_lowercase();
    let tokens = tokenize(&q);

    if is_missing(&q) {
        return missing_subgraph(graph);
    }

    let impact = is_impact(&q);
    let crate_id = resolve_crate(graph, &tokens);

    if impact {
        if let Some(cid) = &crate_id {
            let seed = NodeRef::new(EntityKind::Crate, cid);
            let rels: BTreeSet<RelationKind> =
                [RelationKind::DependsOn, RelationKind::CompilesTo].into_iter().collect();
            return neighborhood::neighborhood_multi(
                graph,
                &[seed],
                6,
                Direction::In,
                Some(&rels),
            );
        }
    }

    if let Some(slug) = resolve_champion(graph, &tokens) {
        let seed = NodeRef::new(EntityKind::Champion, slug);
        return get_neighborhood(graph, &seed, 3, Direction::Out);
    }

    if let Some(cid) = crate_id {
        let seed = NodeRef::new(EntityKind::Crate, cid);
        return get_neighborhood(graph, &seed, 4, Direction::Out);
    }

    Subgraph::empty()
}

/// Answer a natural-language question, GROUNDED in the retrieved subgraph.
///
/// Retrieves -> serializes -> calls `deps.ask` -> filters the returned citations
/// (and derived `used_nodes`) to entities that are actually in the subgraph.
/// Anything the model fabricated — or any real graph node that wasn't retrieved —
/// is dropped, exactly the discipline the domain GraphRAG enforces.
pub fn answer<A: Ask + ?Sized>(graph: &Graph, question: &str, deps: &A) -> RagAnswer {
    let subgraph = retrieve(graph, question);
    let context = serialize_for_llm(&subgraph);

    let resp = match deps.ask(SYSTEM_PROMPT, &context, question) {
        Ok(r) => r,
        Err(e) => {
            return RagAnswer {
                answer: format!("LLM error: {e}"),
                citations: Vec::new(),
                used_nodes: Vec::new(),
                subgraph,
            }
        }
    };

    let valid_nodes = subgraph.node_keys();
    let valid_edges = subgraph.edge_keys();

    let mut citations: Vec<String> = Vec::new();
    let mut used_nodes: Vec<NodeRef> = Vec::new();
    let mut seen: BTreeSet<String> = BTreeSet::new();

    for raw in resp.citations {
        let c = raw.trim().to_string();
        if !seen.insert(c.clone()) {
            continue;
        }
        if valid_nodes.contains(&c) {
            citations.push(c.clone());
            // Resolve back to the handle (grounded: it is in the subgraph).
            if let Some(n) = subgraph.nodes.iter().find(|n| node_key(&n.node_ref()) == c) {
                used_nodes.push(n.node_ref());
            }
        } else if valid_edges.contains(&c) {
            citations.push(c);
        }
        // else: fabricated or out-of-subgraph — DROP it.
    }

    RagAnswer {
        answer: resp.answer,
        citations,
        used_nodes,
        subgraph,
    }
}

/// Parse a `{"answer": ..., "citations": [...]}` object out of raw LLM text,
/// tolerating surrounding prose or code fences. Shared by every [`Ask`] impl.
pub fn parse_llm_json(text: &str) -> Option<LlmResponse> {
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    if end < start {
        return None;
    }
    let slice = &text[start..=end];
    let val: serde_json::Value = serde_json::from_str(slice).ok()?;
    let answer = val.get("answer").and_then(|a| a.as_str())?.to_string();
    let citations = val
        .get("citations")
        .and_then(|c| c.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|c| c.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    Some(LlmResponse { answer, citations })
}

// --- intent detection & entity resolution -----------------------------------

fn tokenize(q: &str) -> Vec<String> {
    q.split(|c: char| !c.is_alphanumeric() && c != '-')
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

fn is_missing(q: &str) -> bool {
    ["missing", "incomplete", "lacking", "without a", "no model", "untested"]
        .iter()
        .any(|k| q.contains(k))
}

fn is_impact(q: &str) -> bool {
    ["break", "breaks", "impact", "affect", "blast", "ripple", "change", "changing", "edit"]
        .iter()
        .any(|k| q.contains(k))
}

/// First token that resolves to a real crate id (short/dir/full name).
fn resolve_crate(graph: &Graph, tokens: &[String]) -> Option<String> {
    tokens.iter().find_map(|t| query::resolve_crate(graph, t))
}

/// First token that matches a champion slug (or its lowercased name).
fn resolve_champion(graph: &Graph, tokens: &[String]) -> Option<String> {
    for champ in graph.nodes_of(EntityKind::Champion) {
        let name = champ.props.get("name").map(|s| s.to_lowercase());
        for t in tokens {
            if *t == champ.id || name.as_deref() == Some(t.as_str()) {
                return Some(champ.id.clone());
            }
        }
    }
    None
}

/// The completeness pass as a subgraph: the incomplete champions' bundles.
fn missing_subgraph(graph: &Graph) -> Subgraph {
    let seeds: Vec<NodeRef> = query::missing(graph)
        .into_iter()
        .map(|g| NodeRef::new(EntityKind::Champion, g.slug))
        .collect();
    if seeds.is_empty() {
        return Subgraph::empty();
    }
    neighborhood::neighborhood_multi(graph, &seeds, 3, Direction::Out, None)
}

//! GraphRAG serving tests: retrieval correctness, deterministic serialization,
//! grounding discipline, and impact-query seeding — all against the REAL repo
//! graph, with the LLM stubbed by a mock so no API key is needed.

use std::path::PathBuf;

use aeonrift_graph::ontology::{EntityKind, NodeRef, RelationKind};
use aeonrift_graph::rag::{self, Ask, Direction, LlmResponse};
use aeonrift_graph::{build, Graph};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root must resolve")
}

fn graph() -> Graph {
    build(&repo_root()).expect("extraction over the real repo must succeed")
}

fn crate_ref(id: &str) -> NodeRef {
    NodeRef::new(EntityKind::Crate, id)
}

// ---------------------------------------------------------------------------
// (a) retrieval correctness
// ---------------------------------------------------------------------------

#[test]
fn reverse_neighborhood_of_sim_core_contains_its_dependents() {
    let g = graph();
    let seed = crate_ref("aeonrift-sim-core");
    let sub = rag::get_neighborhood(&g, &seed, 6, Direction::In);

    // Everything that (transitively) depends on the sim core must be reachable
    // by walking DEPENDS_ON backwards.
    for dependent in [
        "aeonrift-combat",
        "aeonrift-ability-dsl",
        "aeonrift-net",
        "aeonrift-ai-bots",
        "aeonrift-balance-sim",
        "aeonrift-moba",
        "aeonrift-ascendancy",
    ] {
        assert!(
            sub.contains_node(&crate_ref(dependent)),
            "reverse neighborhood of sim-core should include {dependent}"
        );
    }
    // The seed itself is present; the walk carried real DEPENDS_ON edges.
    assert!(sub.contains_node(&seed));
    assert!(sub.has_rel(RelationKind::DependsOn));
}

#[test]
fn champion_neighborhood_is_the_full_bundle_without_sibling_bleed() {
    let g = graph();
    let seed = NodeRef::new(EntityKind::Champion, "achilles");
    let sub = rag::get_neighborhood(&g, &seed, 3, Direction::Out);

    // The bundle spans code + data + art.
    for rel in [
        RelationKind::HasAbility,   // abilities
        RelationKind::UsesEffect,   // effects
        RelationKind::BelongsTo,    // faction
        RelationKind::CompilesTo,   // the games
        RelationKind::ReferencesAsset,
        RelationKind::Manifests,
        RelationKind::StoredIn,     // assets -> store
    ] {
        assert!(
            sub.has_rel(rel),
            "achilles bundle missing {}",
            rel.verb()
        );
    }
    // Forward-directed: no sibling champion bleeds in as a subject.
    for e in &sub.edges {
        if e.from.kind == EntityKind::Champion {
            assert_eq!(e.from.id, "achilles", "sibling bled in: {:?}", e.from);
        }
    }
}

#[test]
fn champion_neighborhood_respects_the_hop_bound() {
    let g = graph();
    let seed = NodeRef::new(EntityKind::Champion, "achilles");

    // Depth 1: abilities present, but their effects (hop 2) and assets (hop 2)
    // are not yet reached.
    let d1 = rag::get_neighborhood(&g, &seed, 1, Direction::Out);
    assert!(d1.has_rel(RelationKind::HasAbility));
    assert!(!d1.has_rel(RelationKind::UsesEffect), "effects are 2 hops away");
    assert!(!d1.has_rel(RelationKind::Manifests), "assets are 2 hops away");

    // Depth 2: effects and the manifest's assets appear; the store (hop 3) has not.
    let d2 = rag::get_neighborhood(&g, &seed, 2, Direction::Out);
    assert!(d2.has_rel(RelationKind::UsesEffect));
    assert!(d2.has_rel(RelationKind::Manifests));
    assert!(!d2.has_rel(RelationKind::StoredIn), "asset store is 3 hops away");

    // Depth 3: the asset store is reached. Bounded BFS grows monotonically.
    let d3 = rag::get_neighborhood(&g, &seed, 3, Direction::Out);
    assert!(d3.has_rel(RelationKind::StoredIn));
    assert!(d3.nodes.len() >= d2.nodes.len());
    assert!(d2.nodes.len() >= d1.nodes.len());
}

// ---------------------------------------------------------------------------
// (b) deterministic, order-independent serialization
// ---------------------------------------------------------------------------

#[test]
fn serialization_is_byte_identical_and_order_independent() {
    let g = graph();
    let seed = NodeRef::new(EntityKind::Champion, "achilles");

    // Two independent retrievals of the same neighborhood must serialize equally.
    let a = rag::serialize_for_llm(&rag::get_neighborhood(&g, &seed, 3, Direction::Out));
    let b = rag::serialize_for_llm(&rag::get_neighborhood(&g, &seed, 3, Direction::Out));
    assert_eq!(a, b, "serialization must be byte-identical across runs");

    // A rebuilt graph (fresh extraction) yields the same subgraph text — proving
    // the output depends only on graph content, not build/visitation order.
    let g2 = graph();
    let c = rag::serialize_for_llm(&rag::get_neighborhood(&g2, &seed, 3, Direction::Out));
    assert_eq!(a, c, "serialization must be independent of build order");

    // Sanity: the serialized form actually carries the expected sections.
    assert!(a.contains("## NODES"));
    assert!(a.contains("## EDGES"));
    assert!(a.contains("## PROVENANCE"));
    assert!(a.contains("Champion:achilles"));
}

// ---------------------------------------------------------------------------
// (c) grounding: fabricated / out-of-subgraph citations are dropped
// ---------------------------------------------------------------------------

/// A mock LLM that returns a fixed answer plus a mix of citations: one that IS
/// in the retrieved subgraph, one real graph node that is NOT (a crate — a
/// champion's forward bundle never reaches crates), and one wholly fabricated.
struct MockAsk;

impl Ask for MockAsk {
    fn ask(&self, _system: &str, _context: &str, _question: &str) -> Result<LlmResponse, String> {
        Ok(LlmResponse {
            answer: "Achilles is grounded in the subgraph.".to_string(),
            citations: vec![
                "Champion:achilles".to_string(),          // in-subgraph  -> KEEP
                "Crate:aeonrift-sim-core".to_string(),    // real node, not retrieved -> DROP
                "Champion:__fabricated__".to_string(),    // does not exist -> DROP
            ],
        })
    }
}

#[test]
fn answer_grounds_citations_to_the_retrieved_subgraph() {
    let g = graph();
    let ans = rag::answer(&g, "what does champion achilles touch?", &MockAsk);

    // Prose passes through untouched.
    assert_eq!(ans.answer, "Achilles is grounded in the subgraph.");

    // The fabricated and out-of-subgraph citations were dropped.
    assert!(
        !ans.citations.iter().any(|c| c == "Champion:__fabricated__"),
        "fabricated citation must be dropped"
    );
    assert!(
        !ans.citations.iter().any(|c| c == "Crate:aeonrift-sim-core"),
        "a real node absent from the subgraph must be dropped"
    );
    assert!(ans.citations.contains(&"Champion:achilles".to_string()));

    // EVERY surviving citation is provably a node or edge of the subgraph.
    let node_keys = ans.subgraph.node_keys();
    let edge_keys = ans.subgraph.edge_keys();
    for c in &ans.citations {
        assert!(
            node_keys.contains(c) || edge_keys.contains(c),
            "surviving citation {c} is not in the retrieved subgraph"
        );
    }

    // used_nodes are the resolved handles of kept node citations — all grounded.
    for nr in &ans.used_nodes {
        assert!(ans.subgraph.contains_node(nr), "used node {nr:?} not in subgraph");
    }
    assert!(ans
        .used_nodes
        .contains(&NodeRef::new(EntityKind::Champion, "achilles")));
}

// ---------------------------------------------------------------------------
// (d) impact query: seeds the named crate, expands reverse DEPENDS_ON
// ---------------------------------------------------------------------------

#[test]
fn impact_query_seeds_the_crate_and_covers_the_blast_radius() {
    let g = graph();
    let sub = rag::retrieve(&g, "what breaks if I change sim-core?");

    // The impact intent must resolve sim-core and expand its dependents.
    assert!(!sub.is_empty(), "impact query should resolve a subgraph");
    for dependent in [
        "aeonrift-combat",
        "aeonrift-ability-dsl",
        "aeonrift-moba",       // the games...
        "aeonrift-ascendancy", // ...both of them
    ] {
        assert!(
            sub.contains_node(&crate_ref(dependent)),
            "impact of changing sim-core must include {dependent}"
        );
    }

    // And the same question runs end-to-end through the grounded answer path.
    let ans = rag::answer(&g, "what breaks if I change sim-core?", &MockAsk);
    assert!(ans.citations.contains(&"Champion:achilles".to_string()) == false);
    // sim-core IS the seed here, so it is legitimately in this subgraph.
    assert!(ans
        .citations
        .contains(&"Crate:aeonrift-sim-core".to_string()));
}

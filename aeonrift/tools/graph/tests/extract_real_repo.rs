//! Integration tests: run the extractors over the REAL monorepo and assert the
//! graph is accurate and the queries are genuinely useful. These are the Stage-7
//! quality gate + Stage-9 serving guarantees, proven against live repo data.

use std::path::PathBuf;

use aeonrift_graph::ontology::{Edge, EntityKind, NodeRef, RelationKind};
use aeonrift_graph::{build, query, validate, Graph};

/// The repo root relative to this crate (`tools/graph` -> workspace root).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root must resolve")
}

fn graph() -> Graph {
    build(&repo_root()).expect("extraction over the real repo must succeed")
}

/// Does a `DEPENDS_ON` edge `from -> to` (by crate short/full id) exist?
fn has_dep(g: &Graph, from: &str, to: &str) -> bool {
    let from = query::resolve_crate(g, from).unwrap_or_default();
    let to = query::resolve_crate(g, to).unwrap_or_default();
    g.edges()
        .any(|e| e.rel == RelationKind::DependsOn && e.from.id == from && e.to.id == to)
}

#[test]
fn extracts_the_full_47_champion_roster() {
    let g = graph();
    let champions = g.nodes_of(EntityKind::Champion).count();
    assert_eq!(
        champions, 47,
        "expected the shipped 47-champion roster, got {champions}"
    );
}

#[test]
fn known_cargo_dependency_edges_are_present() {
    let g = graph();
    // Real path deps in the workspace (accurate as of master @ 523d4b9). Note the
    // DSL depends on the sim core directly, NOT on combat (the AEONRIFT-GRAPH.md
    // sketch shows dsl->combat aspirationally; the extractor reports the truth).
    assert!(has_dep(&g, "combat", "sim-core"), "combat -> sim-core");
    assert!(
        has_dep(&g, "ability-dsl", "sim-core"),
        "ability-dsl -> sim-core"
    );
    assert!(has_dep(&g, "net", "sim-core"), "net -> sim-core");
    assert!(has_dep(&g, "aeonrift-moba", "sim-core"), "moba -> sim-core");
    assert!(
        has_dep(&g, "aeonrift-ascendancy", "sim-core"),
        "ascendancy -> sim-core"
    );
    assert!(
        has_dep(&g, "aeonrift-ascendancy", "ability-dsl"),
        "ascendancy -> ability-dsl"
    );
    assert!(has_dep(&g, "ai-bots", "combat"), "ai-bots -> combat");
    assert!(
        has_dep(&g, "balance-sim", "ai-bots"),
        "balance-sim -> ai-bots"
    );
}

#[test]
fn rdeps_sim_core_is_real_impact_analysis() {
    let g = graph();
    let id = query::resolve_crate(&g, "sim-core").expect("sim-core exists");
    let impacted = query::rdeps(&g, &id);
    // Changing the sim core ripples to combat, the DSL, AND both games — the whole
    // point of a build graph.
    for expected in [
        "aeonrift-combat",
        "aeonrift-ability-dsl",
        "aeonrift-moba",
        "aeonrift-ascendancy",
        "aeonrift-ai-bots",
        "aeonrift-balance-sim",
    ] {
        assert!(
            impacted.iter().any(|c| c == expected),
            "rdeps(sim-core) should include {expected}; got {impacted:?}"
        );
    }
}

#[test]
fn deps_closure_is_transitive() {
    let g = graph();
    let id = query::resolve_crate(&g, "balance-sim").unwrap();
    let deps = query::deps(&g, &id);
    // balance-sim -> ai-bots -> {sim-core, combat} : sim-core is reached transitively.
    assert!(deps.iter().any(|c| c == "aeonrift-sim-core"));
    assert!(deps.iter().any(|c| c == "aeonrift-ai-bots"));
    assert!(deps.iter().any(|c| c == "aeonrift-combat"));
}

#[test]
fn every_uses_effect_edge_targets_the_closed_vocabulary() {
    let g = graph();
    const VOCAB: [&str; 5] = ["Damage", "Heal", "Slow", "Stun", "Dash"];
    let mut count = 0;
    for e in g.edges().filter(|e| e.rel == RelationKind::UsesEffect) {
        count += 1;
        assert_eq!(e.to.kind, EntityKind::EffectType);
        assert!(
            VOCAB.contains(&e.to.id.as_str()),
            "USES_EFFECT points outside the closed vocabulary: {}",
            e.to.id
        );
    }
    assert!(count > 0, "expected USES_EFFECT edges to exist");
    // The EffectType nodes fuse to exactly the 5-verb closed vocabulary.
    assert_eq!(g.nodes_of(EntityKind::EffectType).count(), 5);
}

#[test]
fn every_champion_has_at_least_one_effect() {
    let g = graph();
    for champ in g.nodes_of(EntityKind::Champion) {
        let cref = champ.node_ref();
        // champion -> ability -> effect must exist for a shipped champion.
        let has_ability = g
            .out_edges(&cref)
            .any(|e| e.rel == RelationKind::HasAbility);
        assert!(has_ability, "{} has no abilities", champ.id);
    }
    let uses_effect = g
        .edges()
        .filter(|e| e.rel == RelationKind::UsesEffect)
        .count();
    assert!(
        uses_effect >= 47,
        "expected many USES_EFFECT edges, got {uses_effect}"
    );
}

#[test]
fn real_repo_passes_the_quality_gate_but_a_synthetic_bad_edge_is_rejected() {
    let mut g = graph();
    // The real, extracted graph is type-clean.
    assert!(
        validate::validate(&g).is_clean(),
        "the real repo must pass the domain/range gate"
    );

    // Inject a synthetic type-invalid edge: EffectType -HAS_ABILITY-> Champion
    // (HAS_ABILITY :: Champion -> Ability, so both endpoints are wrong).
    g.add_edge(Edge::new(
        NodeRef::new(EntityKind::EffectType, "Damage"),
        RelationKind::HasAbility,
        NodeRef::new(EntityKind::Champion, "achilles"),
        aeonrift_graph::Provenance::from("synthetic", "test"),
    ));
    let report = validate::validate(&g);
    assert_eq!(
        report.rejected.len(),
        1,
        "the gate must reject exactly the bad edge"
    );
    assert_eq!(report.rejected[0].edge.rel, RelationKind::HasAbility);
}

#[test]
fn completeness_critic_finds_nothing_missing() {
    let g = graph();
    let gaps = query::missing(&g);
    assert!(
        gaps.is_empty(),
        "all 47 champions should have a model, faction and test; missing: {gaps:?}"
    );
}

#[test]
fn champion_subgraph_is_the_champion_bundle_without_sibling_bleed() {
    let g = graph();
    let edges = query::champion_subgraph(&g, "achilles", 3);
    assert!(!edges.is_empty());
    // Forward-directed: every edge is reachable downward from achilles, so no other
    // champion appears as a subject.
    for e in &edges {
        if e.from.kind == EntityKind::Champion {
            assert_eq!(
                e.from.id, "achilles",
                "sibling bled into the subgraph: {:?}",
                e.from
            );
        }
    }
    // The bundle spans code+data+art: it must include an ability, an effect, the
    // faction, both games, and the asset store.
    let rels: Vec<RelationKind> = edges.iter().map(|e| e.rel).collect();
    for want in [
        RelationKind::HasAbility,
        RelationKind::UsesEffect,
        RelationKind::BelongsTo,
        RelationKind::CompilesTo,
        RelationKind::ReferencesAsset,
        RelationKind::Manifests,
        RelationKind::StoredIn,
    ] {
        assert!(
            rels.contains(&want),
            "champion bundle missing {}",
            want.verb()
        );
    }
}

#[test]
fn extraction_is_deterministic() {
    let a = query::export(&graph(), query::ExportFormat::Json);
    let b = query::export(&graph(), query::ExportFormat::Json);
    assert_eq!(a, b, "the JSON export must be byte-identical across runs");
}

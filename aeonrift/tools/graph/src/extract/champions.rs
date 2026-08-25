//! Champion extractor: the roster subgraph.
//!
//! Loads every `content/champions/*.ron` (excluding the `roster_meta.ron` sidecar)
//! THROUGH the real `aeonrift-ability-dsl` loader — so the graph's closed effect
//! vocabulary is literally the game's, and a champion that would not compile also
//! does not enter the graph. Emits, per champion:
//!   - `Champion` node + `HAS_ABILITY` -> `Ability` + `USES_EFFECT` -> `EffectType`
//!   - `BELONGS_TO` -> `Faction` (from `roster_meta.ron`)
//!   - `COMPILES_TO` -> both `Game`s (shared-roster invariant, as a queryable edge)
//!   - `VALIDATED_BY` -> `Schema` and `TESTED_BY` -> `Gate` (the CI gates it passes)

use std::collections::BTreeMap;
use std::path::Path;

use champions::{load_file, Effect};
use serde::Deserialize;

use crate::extract::anyhow_lite::{Context, Result};
use crate::extract::ids;
use crate::graph::Graph;
use crate::ontology::{Edge, EntityKind, Node, NodeRef, Provenance, RelationKind};

/// Deserialization mirror of `content/champions/roster_meta.ron`.
#[derive(Debug, Deserialize)]
#[serde(rename = "RosterMeta")]
struct RawRosterMeta(BTreeMap<String, RawRosterEntry>);

#[derive(Debug, Deserialize)]
struct RawRosterEntry {
    archetype: String,
    faction: String,
}

/// Extract the champion roster subgraph.
pub fn extract(repo_root: &Path, graph: &mut Graph) -> Result<()> {
    let champions_dir = repo_root.join("content/champions");
    let roster = load_roster_meta(&champions_dir)?;

    // Product / gate singleton nodes referenced by every champion.
    add_singletons(repo_root, graph);

    let mut paths: Vec<_> = std::fs::read_dir(&champions_dir)
        .context(format!("reading {}", champions_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|x| x == "ron").unwrap_or(false))
        .filter(|p| p.file_stem().and_then(|s| s.to_str()) != Some("roster_meta"))
        .collect();
    paths.sort();

    for path in paths {
        let slug = path
            .file_stem()
            .and_then(|s| s.to_str())
            .context("champion file has no stem")?
            .to_string();
        let spec = load_file(&path)
            .map_err(|e| format!("champion {slug} failed the DSL loader gate: {e}"))?;
        let source = format!("content/champions/{slug}.ron");

        // Champion node (author is human — this repo's specs are hand-authored data).
        let mut champ = Node::new(
            EntityKind::Champion,
            &slug,
            Provenance::from(&source, "human"),
        )
        .with_prop("name", &spec.name)
        .with_prop("abilities", spec.abilities.len().to_string());
        if let Some(meta) = roster.get(&slug) {
            champ = champ
                .with_prop("archetype", &meta.archetype)
                .with_prop("faction", &meta.faction);
        }
        graph.add_node(champ);
        let champ_ref = NodeRef::new(EntityKind::Champion, &slug);

        // Abilities + effects.
        for ability in &spec.abilities {
            let ability_id = format!("{slug}:{}", ability.name);
            graph.add_node(
                Node::new(
                    EntityKind::Ability,
                    &ability_id,
                    Provenance::from(&source, "ability-dsl-loader"),
                )
                .with_prop("name", &ability.name)
                .with_prop("champion", &slug)
                .with_prop("targeting", format!("{:?}", ability.targeting)),
            );
            let ability_ref = NodeRef::new(EntityKind::Ability, &ability_id);
            graph.add_edge(Edge::new(
                champ_ref.clone(),
                RelationKind::HasAbility,
                ability_ref.clone(),
                Provenance::from(&source, "ability-dsl-loader"),
            ));

            for effect in &ability.effects {
                let effect_name = effect_variant(effect);
                // EffectType nodes fuse to the closed 5-verb vocabulary.
                graph.add_node(Node::new(
                    EntityKind::EffectType,
                    effect_name,
                    Provenance::from("engine/ability-dsl/src/spec.rs", "closed-vocabulary"),
                ));
                graph.add_edge(Edge::new(
                    ability_ref.clone(),
                    RelationKind::UsesEffect,
                    NodeRef::new(EntityKind::EffectType, effect_name),
                    Provenance::from(&source, "ability-dsl-loader"),
                ));
            }
        }

        // BELONGS_TO faction (from the roster_meta sidecar).
        if let Some(meta) = roster.get(&slug) {
            graph.add_node(Node::new(
                EntityKind::Faction,
                &meta.faction,
                Provenance::from("content/champions/roster_meta.ron", "content-parse"),
            ));
            graph.add_edge(Edge::new(
                champ_ref.clone(),
                RelationKind::BelongsTo,
                NodeRef::new(EntityKind::Faction, &meta.faction),
                Provenance::from("content/champions/roster_meta.ron", "content-parse"),
            ));
        }

        // COMPILES_TO both games (both reuse aeonrift-ability-dsl / the roster).
        for game in [ids::GAME_MOBA, ids::GAME_ASCENDANCY] {
            graph.add_edge(Edge::new(
                champ_ref.clone(),
                RelationKind::CompilesTo,
                NodeRef::new(EntityKind::Game, game),
                Provenance::from("engine/ability-dsl/src/interpreter.rs", "reuse-inference"),
            ));
        }

        // VALIDATED_BY the champion schema (the loader enforces the closed vocabulary).
        graph.add_edge(Edge::new(
            champ_ref.clone(),
            RelationKind::ValidatedBy,
            NodeRef::new(EntityKind::Schema, ids::CHAMPION_SCHEMA),
            Provenance::from("content/_schema/champion.schema.ron", "content-lint"),
        ));

        // TESTED_BY the deterministic roster duel/balance gate (roster.rs duels every spec).
        graph.add_edge(Edge::new(
            champ_ref,
            RelationKind::TestedBy,
            NodeRef::new(EntityKind::Gate, ids::ROSTER_GATE),
            Provenance::from("engine/ability-dsl/tests/roster.rs", "duel-harness"),
        ));
    }
    Ok(())
}

fn add_singletons(_repo_root: &Path, graph: &mut Graph) {
    graph.add_node(
        Node::new(
            EntityKind::Game,
            ids::GAME_MOBA,
            Provenance::from("games/aeonrift-moba/Cargo.toml", "reuse-inference"),
        )
        .with_prop("genre", "MOBA"),
    );
    graph.add_node(
        Node::new(
            EntityKind::Game,
            ids::GAME_ASCENDANCY,
            Provenance::from("games/aeonrift-ascendancy/Cargo.toml", "reuse-inference"),
        )
        .with_prop("genre", "card-battler"),
    );
    graph.add_node(Node::new(
        EntityKind::Schema,
        ids::CHAMPION_SCHEMA,
        Provenance::from("content/_schema/champion.schema.ron", "content-parse"),
    ));
    graph.add_node(
        Node::new(
            EntityKind::Gate,
            ids::ROSTER_GATE,
            Provenance::from("engine/ability-dsl/tests/roster.rs", "content-parse"),
        )
        .with_prop("kind", "deterministic round-robin duel + validation"),
    );
}

/// Map a DSL [`Effect`] to its closed-vocabulary variant name.
fn effect_variant(effect: &Effect) -> &'static str {
    match effect {
        Effect::Damage { .. } => "Damage",
        Effect::Heal { .. } => "Heal",
        Effect::Slow { .. } => "Slow",
        Effect::Stun { .. } => "Stun",
        Effect::Dash { .. } => "Dash",
    }
}

fn load_roster_meta(champions_dir: &Path) -> Result<BTreeMap<String, RawRosterEntry>> {
    let path = champions_dir.join("roster_meta.ron");
    let text = std::fs::read_to_string(&path).context(format!("reading {}", path.display()))?;
    let meta: RawRosterMeta = ron::from_str(&text).context("parsing roster_meta.ron")?;
    Ok(meta.0)
}

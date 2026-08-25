//! Stage 3 — the ontology: typed entity + relation kinds, each relation carrying a
//! precise `domain() -> range()` type signature, plus the `Node`/`Edge`/`Provenance`
//! value types.
//!
//! Two knowledge-graph course rules are baked in here:
//!   1. Every relation is a **precise verb** (`COMPILES_TO`, never `RELATED_TO`).
//!   2. Every edge is **type-checked** against `domain()`/`range()` — an edge whose
//!      endpoints violate the signature is rejected by [`crate::validate`] exactly the
//!      way a knowledge graph kills hallucinated structure.
//!
//! Everything here is ordered/deterministic: enums derive `Ord` (declaration order),
//! props live in `BTreeMap`, so serialized output is byte-stable across runs.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The 23 node kinds of the AEONRIFT repo ontology (AEONRIFT-GRAPH.md, Stage 3).
///
/// Declaration order is significant: it defines the deterministic sort order used
/// throughout export and querying (via the derived `Ord`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum EntityKind {
    Repo,
    Workspace,
    Crate,
    Module,
    Game,
    Genre,
    Champion,
    Ability,
    EffectType,
    ContentType,
    Schema,
    ContentFile,
    Asset,
    AssetManifest,
    AssetStore,
    Faction,
    Pantheon,
    Locale,
    Gate,
    Tool,
    Service,
    Rail,
    Domain,
}

/// The 17 relation kinds. Each is a precise verb with a typed `domain -> range`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RelationKind {
    Contains,
    DependsOn,
    CompilesTo,
    Reuses,
    HasAbility,
    UsesEffect,
    BelongsTo,
    ValidatedBy,
    TestedBy,
    GeneratedBy,
    Manifests,
    ReferencesAsset,
    StoredIn,
    Localizes,
    ServedBy,
    SettlesOn,
    HostsCompliance,
}

impl RelationKind {
    /// The SCREAMING_SNAKE_CASE verb as it appears in AEONRIFT-GRAPH.md.
    pub const fn verb(self) -> &'static str {
        use RelationKind::*;
        match self {
            Contains => "CONTAINS",
            DependsOn => "DEPENDS_ON",
            CompilesTo => "COMPILES_TO",
            Reuses => "REUSES",
            HasAbility => "HAS_ABILITY",
            UsesEffect => "USES_EFFECT",
            BelongsTo => "BELONGS_TO",
            ValidatedBy => "VALIDATED_BY",
            TestedBy => "TESTED_BY",
            GeneratedBy => "GENERATED_BY",
            Manifests => "MANIFESTS",
            ReferencesAsset => "REFERENCES_ASSET",
            StoredIn => "STORED_IN",
            Localizes => "LOCALIZES",
            ServedBy => "SERVED_BY",
            SettlesOn => "SETTLES_ON",
            HostsCompliance => "HOSTS_COMPLIANCE",
        }
    }

    /// Allowed source (tail) entity kinds for this relation.
    pub const fn domain(self) -> &'static [EntityKind] {
        use EntityKind::*;
        use RelationKind::*;
        match self {
            Contains => &[Repo, Workspace, Crate, ContentType],
            DependsOn => &[Crate],
            CompilesTo => &[Champion],
            Reuses => &[Game, Champion],
            HasAbility => &[Champion],
            UsesEffect => &[Ability],
            BelongsTo => &[Champion],
            ValidatedBy => &[ContentFile, Champion],
            TestedBy => &[Champion],
            GeneratedBy => &[Champion, Asset],
            Manifests => &[AssetManifest],
            ReferencesAsset => &[Champion],
            StoredIn => &[Asset],
            Localizes => &[Locale],
            ServedBy => &[Game],
            SettlesOn => &[Game, Asset],
            HostsCompliance => &[Domain],
        }
    }

    /// Allowed target (head) entity kinds for this relation.
    pub const fn range(self) -> &'static [EntityKind] {
        use EntityKind::*;
        use RelationKind::*;
        match self {
            Contains => &[Workspace, Crate, Module, ContentFile],
            DependsOn => &[Crate],
            CompilesTo => &[Game, Crate],
            Reuses => &[Crate, Champion],
            HasAbility => &[Ability],
            UsesEffect => &[EffectType],
            BelongsTo => &[Faction, Pantheon],
            ValidatedBy => &[Schema, Gate],
            TestedBy => &[Gate],
            GeneratedBy => &[Tool],
            Manifests => &[Asset],
            ReferencesAsset => &[AssetManifest],
            StoredIn => &[AssetStore],
            Localizes => &[ContentType],
            ServedBy => &[Service],
            SettlesOn => &[Rail],
            HostsCompliance => &[Game],
        }
    }

    /// `true` when `from -> to` satisfies both this relation's domain and range.
    pub fn accepts(self, from: EntityKind, to: EntityKind) -> bool {
        self.domain().contains(&from) && self.range().contains(&to)
    }
}

/// A `(kind, id)` handle to a node. Nodes are identified by this pair — an `id`
/// alone is NOT globally unique (Champion `achilles` and AssetManifest `achilles`
/// share an id but are distinct nodes).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct NodeRef {
    pub kind: EntityKind,
    pub id: String,
}

impl NodeRef {
    pub fn new(kind: EntityKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }
}

/// Stage-8 provenance carried by every node and edge: where the fact came from,
/// how it was derived, and (for content-addressed assets) its blake3 hash.
///
/// `extracted_at` is intentionally left unset by the extractors so that repeated
/// runs over an unchanged repo produce byte-identical output (determinism > wall
/// clock). It exists for callers that want to stamp a run.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
pub struct Provenance {
    /// The file the fact was read from (Cargo.toml / champion .ron / manifest).
    pub source: String,
    /// How the fact was derived: `human`, `cargo-parse`, `ability-dsl-loader`,
    /// `blender`, `reuse-inference`, ...
    #[serde(skip_serializing_if = "Option::is_none")]
    pub generator: Option<String>,
    /// Content-addressed hash for assets (blake3), when applicable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// Optional run timestamp; unset by extractors for deterministic output.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extracted_at: Option<String>,
}

impl Provenance {
    /// Provenance from a source file plus a generator label.
    pub fn from(source: impl Into<String>, generator: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            generator: Some(generator.into()),
            hash: None,
            extracted_at: None,
        }
    }
}

/// A typed node: a `(kind, id)` identity plus ordered string props and provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Node {
    pub id: String,
    pub kind: EntityKind,
    pub props: BTreeMap<String, String>,
    pub provenance: Provenance,
}

impl Node {
    pub fn new(kind: EntityKind, id: impl Into<String>, provenance: Provenance) -> Self {
        Self {
            id: id.into(),
            kind,
            props: BTreeMap::new(),
            provenance,
        }
    }

    /// Builder-style prop insertion (immutable-friendly: returns a new value).
    pub fn with_prop(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.props.insert(key.into(), value.into());
        self
    }

    /// This node's `(kind, id)` handle.
    pub fn node_ref(&self) -> NodeRef {
        NodeRef::new(self.kind, self.id.clone())
    }
}

/// A typed, directed, provenance-carrying edge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Edge {
    pub from: NodeRef,
    pub rel: RelationKind,
    pub to: NodeRef,
    pub provenance: Provenance,
}

impl Edge {
    pub fn new(from: NodeRef, rel: RelationKind, to: NodeRef, provenance: Provenance) -> Self {
        Self {
            from,
            rel,
            to,
            provenance,
        }
    }

    /// Deduplication / ordering key: `(from, rel, to)` (provenance excluded).
    pub fn key(&self) -> (NodeRef, RelationKind, NodeRef) {
        (self.from.clone(), self.rel, self.to.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_relation_has_nonempty_domain_and_range() {
        // 17 relation kinds, each with a real type signature.
        let rels = [
            RelationKind::Contains,
            RelationKind::DependsOn,
            RelationKind::CompilesTo,
            RelationKind::Reuses,
            RelationKind::HasAbility,
            RelationKind::UsesEffect,
            RelationKind::BelongsTo,
            RelationKind::ValidatedBy,
            RelationKind::TestedBy,
            RelationKind::GeneratedBy,
            RelationKind::Manifests,
            RelationKind::ReferencesAsset,
            RelationKind::StoredIn,
            RelationKind::Localizes,
            RelationKind::ServedBy,
            RelationKind::SettlesOn,
            RelationKind::HostsCompliance,
        ];
        assert_eq!(rels.len(), 17);
        for r in rels {
            assert!(!r.domain().is_empty(), "{} has empty domain", r.verb());
            assert!(!r.range().is_empty(), "{} has empty range", r.verb());
        }
    }

    #[test]
    fn accepts_enforces_domain_and_range() {
        assert!(RelationKind::UsesEffect.accepts(EntityKind::Ability, EntityKind::EffectType));
        // Champion -USES_EFFECT-> EffectType violates the domain (Ability only).
        assert!(!RelationKind::UsesEffect.accepts(EntityKind::Champion, EntityKind::EffectType));
        // Crate -USES_EFFECT-> Faction violates both.
        assert!(!RelationKind::UsesEffect.accepts(EntityKind::Crate, EntityKind::Faction));
    }
}

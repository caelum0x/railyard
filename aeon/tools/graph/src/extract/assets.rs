//! Asset extractor: the git-vs-store boundary as a path.
//!
//! For each `assets-manifests/champions/<slug>/assets.manifest.ron` emits:
//!   `Champion -REFERENCES_ASSET-> AssetManifest -MANIFESTS-> Asset -STORED_IN-> AssetStore`
//! plus `Asset -GENERATED_BY-> Tool`. The `generator` label and blake3 hash from the
//! manifest are carried into provenance, so a `gltf-fallback` placeholder is never
//! mistaken for real art.

use std::collections::BTreeMap;
use std::path::Path;

use serde::Deserialize;

use crate::extract::anyhow_lite::{Context, Result};
use crate::extract::ids;
use crate::graph::Graph;
use crate::ontology::{Edge, EntityKind, Node, NodeRef, Provenance, RelationKind};

/// Deserialization mirror of `assets.manifest.ron`.
#[derive(Debug, Deserialize)]
#[serde(rename = "AssetManifest")]
struct RawManifest {
    slug: String,
    champion: String,
    #[serde(default)]
    faction: String,
    generator: String,
    assets: BTreeMap<String, RawAssetEntry>,
}

#[derive(Debug, Deserialize)]
#[serde(rename = "AssetEntry")]
struct RawAssetEntry {
    path: String,
    format: String,
    bytes: u64,
    blake3: String,
}

/// Extract the asset subgraph from every champion manifest.
pub fn extract(repo_root: &Path, graph: &mut Graph) -> Result<()> {
    // The single content-addressed store every asset settles into.
    graph.add_node(
        Node::new(
            EntityKind::AssetStore,
            ids::ASSET_STORE,
            Provenance::from("assets-manifests/README.md", "content-parse"),
        )
        .with_prop("backend", "content-addressed R2/CDN blob store"),
    );

    let manifests_dir = repo_root.join("assets-manifests/champions");
    if !manifests_dir.is_dir() {
        return Ok(()); // No manifests yet — nothing to extract.
    }

    let mut slugs: Vec<_> = std::fs::read_dir(&manifests_dir)
        .context(format!("reading {}", manifests_dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    slugs.sort();

    for slug_dir in slugs {
        let manifest_path = slug_dir.join("assets.manifest.ron");
        if !manifest_path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&manifest_path)
            .context(format!("reading {}", manifest_path.display()))?;
        let manifest: RawManifest =
            ron::from_str(&text).context(format!("parsing {}", manifest_path.display()))?;
        let source = format!(
            "assets-manifests/champions/{}/assets.manifest.ron",
            manifest.slug
        );

        extract_manifest(&manifest, &source, graph);
    }
    Ok(())
}

fn extract_manifest(manifest: &RawManifest, source: &str, graph: &mut Graph) {
    let manifest_prov = Provenance::from(source, &manifest.generator);

    // AssetManifest node.
    let mani_ref = NodeRef::new(EntityKind::AssetManifest, &manifest.slug);
    graph.add_node(
        Node::new(
            EntityKind::AssetManifest,
            &manifest.slug,
            manifest_prov.clone(),
        )
        .with_prop("champion", &manifest.champion)
        .with_prop("generator", &manifest.generator)
        .with_prop("faction", &manifest.faction),
    );

    // Champion -REFERENCES_ASSET-> AssetManifest.
    graph.add_edge(Edge::new(
        NodeRef::new(EntityKind::Champion, &manifest.slug),
        RelationKind::ReferencesAsset,
        mani_ref.clone(),
        manifest_prov.clone(),
    ));

    // The generating tool (blender / gltf-fallback / ...).
    graph.add_node(Node::new(
        EntityKind::Tool,
        &manifest.generator,
        Provenance::from(source, "content-parse"),
    ));

    for (logical, entry) in &manifest.assets {
        let asset_id = format!("{}/{}", manifest.slug, entry.path);
        // Provenance carrying the generator + blake3 (Stage-8 audit trail).
        let mut asset_prov = Provenance::from(source, &manifest.generator);
        asset_prov.hash = Some(entry.blake3.clone());

        graph.add_node(
            Node::new(EntityKind::Asset, &asset_id, asset_prov.clone())
                .with_prop("logical", logical)
                .with_prop("format", &entry.format)
                .with_prop("bytes", entry.bytes.to_string())
                .with_prop("blake3", &entry.blake3),
        );
        let asset_ref = NodeRef::new(EntityKind::Asset, &asset_id);

        // AssetManifest -MANIFESTS-> Asset.
        graph.add_edge(Edge::new(
            mani_ref.clone(),
            RelationKind::Manifests,
            asset_ref.clone(),
            asset_prov.clone(),
        ));
        // Asset -STORED_IN-> AssetStore.
        graph.add_edge(Edge::new(
            asset_ref.clone(),
            RelationKind::StoredIn,
            NodeRef::new(EntityKind::AssetStore, ids::ASSET_STORE),
            asset_prov.clone(),
        ));
        // Asset -GENERATED_BY-> Tool.
        graph.add_edge(Edge::new(
            asset_ref,
            RelationKind::GeneratedBy,
            NodeRef::new(EntityKind::Tool, &manifest.generator),
            asset_prov,
        ));
    }
}

//! ASSET PIPELINE VALIDATION.
//!
//! Mirrors `tools/blender/validate_assets.py` on the Rust side so `cargo test`
//! alone proves the 3D asset gate: EVERY champion spec must resolve to a valid,
//! on-disk glTF binary (`.glb`) wired via a per-champion `assets.manifest.ron`.
//!
//! This test is intentionally dependency-free (pure `std`): it does not recompute
//! the BLAKE3 hash (no such crate here) -- the Python validator owns hash
//! integrity. Here we assert structural validity + resolvability:
//!   * every spec has `assets/champions/<slug>/assets.manifest.ron`,
//!   * the manifest points at a `model.glb` that exists,
//!   * that file is a valid glTF binary: magic `glTF`, version 2, length field
//!     matches the actual file size,
//!   * the manifest records an archetype + faction.
//!
//! Greybox `.glb` files are small and committed to the repo; real, high-fidelity
//! skins ship separately to the asset store per the franchise architecture.

use std::fs;
use std::path::{Path, PathBuf};

// Workspace root = ability-dsl crate dir's grandparent (engine/ability-dsl -> engine -> root).
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

fn specs_dir() -> PathBuf {
    workspace_root().join("content").join("champions")
}

// glTF binaries live under assets/champions/<slug>/model.glb.
fn assets_dir() -> PathBuf {
    workspace_root().join("assets").join("champions")
}

// Per-champion manifests live under assets-manifests/champions/<slug>/assets.manifest.ron.
fn manifests_dir() -> PathBuf {
    workspace_root().join("assets-manifests").join("champions")
}

/// Every non-sidecar `*.ron` under specs/, by slug (filename stem).
fn champion_slugs() -> Vec<String> {
    let mut slugs = Vec::new();
    for entry in fs::read_dir(specs_dir()).expect("read specs dir") {
        let path = entry.expect("dir entry").path();
        if path.extension().and_then(|s| s.to_str()) != Some("ron") {
            continue;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        if stem == "roster_meta" {
            continue; // sidecar metadata, not a champion
        }
        slugs.push(stem.to_string());
    }
    slugs.sort();
    slugs
}

/// Assert a byte slice is a valid glTF 2.0 binary container.
fn assert_valid_glb(data: &[u8], slug: &str) {
    assert!(
        data.len() >= 12,
        "{slug}: glb shorter than 12-byte header ({} bytes)",
        data.len()
    );
    assert_eq!(&data[0..4], b"glTF", "{slug}: bad glb magic (expected 'glTF')");
    let version = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    assert_eq!(version, 2, "{slug}: unsupported glTF version {version}");
    let declared = u32::from_le_bytes([data[8], data[9], data[10], data[11]]) as usize;
    assert_eq!(
        declared,
        data.len(),
        "{slug}: glb declared length {declared} != actual {}",
        data.len()
    );
    // First chunk must be the JSON chunk (type 'JSON').
    let json_type = &data[16..20];
    assert_eq!(json_type, b"JSON", "{slug}: first chunk is not JSON");
}

#[test]
fn every_spec_has_a_resolvable_valid_glb() {
    let slugs = champion_slugs();
    assert!(!slugs.is_empty(), "no champion specs found in {:?}", specs_dir());

    for slug in &slugs {
        let dir = assets_dir().join(slug);
        let manifest = manifests_dir().join(slug).join("assets.manifest.ron");
        assert!(
            manifest.exists(),
            "{slug}: missing manifest {}",
            manifest.display()
        );

        let manifest_text = fs::read_to_string(&manifest).expect("read manifest");
        assert!(
            manifest_text.contains("archetype:"),
            "{slug}: manifest missing archetype"
        );
        assert!(
            manifest_text.contains("faction:"),
            "{slug}: manifest missing faction"
        );
        assert!(
            manifest_text.contains("blake3:"),
            "{slug}: manifest missing blake3 hash"
        );
        assert!(
            manifest_text.contains("model.glb"),
            "{slug}: manifest does not reference model.glb"
        );

        let model = dir.join("model.glb");
        assert!(
            model.exists(),
            "{slug}: model path does not resolve: {}",
            model.display()
        );
        let data = fs::read(&model).expect("read model.glb");
        assert_valid_glb(&data, slug);
    }
}

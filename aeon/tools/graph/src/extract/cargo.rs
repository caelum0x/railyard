//! Cargo extractor: the build graph.
//!
//! Walks the workspace `[workspace].members`, emits one `Crate` node per package,
//! a `CONTAINS` edge from the `Workspace` (itself contained by the `Repo`), and a
//! `DEPENDS_ON` edge for every path dependency. This is the deterministic,
//! LLM-free backbone that powers blast-radius / impact analysis.

use std::path::Path;

use crate::extract::anyhow_lite::{Context, Result};
use crate::graph::Graph;
use crate::ontology::{Edge, EntityKind, Node, NodeRef, Provenance, RelationKind};

const REPO_ID: &str = "aeonrift";
const WORKSPACE_ID: &str = "aeonrift";

/// Extract crates + dependency edges from every workspace-member `Cargo.toml`.
pub fn extract(repo_root: &Path, graph: &mut Graph) -> Result<()> {
    let root_manifest = repo_root.join("Cargo.toml");
    let root_text = std::fs::read_to_string(&root_manifest)
        .context(format!("reading {}", root_manifest.display()))?;
    let root: toml::Table = toml::from_str(&root_text).context("parsing root Cargo.toml")?;

    // Repo -> Workspace structural nesting (both concepts exist in the ontology).
    let repo_prov = Provenance::from(rel_path(repo_root, &root_manifest), "cargo-parse");
    graph.add_node(
        Node::new(EntityKind::Repo, REPO_ID, repo_prov.clone())
            .with_prop("name", "aeonrift monorepo"),
    );
    graph.add_node(Node::new(
        EntityKind::Workspace,
        WORKSPACE_ID,
        repo_prov.clone(),
    ));
    graph.add_edge(Edge::new(
        NodeRef::new(EntityKind::Repo, REPO_ID),
        RelationKind::Contains,
        NodeRef::new(EntityKind::Workspace, WORKSPACE_ID),
        repo_prov.clone(),
    ));

    let members = root
        .get("workspace")
        .and_then(|w| w.get("members"))
        .and_then(|m| m.as_array())
        .context("root Cargo.toml has no [workspace].members array")?;

    for member in members {
        let member = member
            .as_str()
            .context("workspace member is not a string")?;
        extract_member(repo_root, member, graph)?;
    }
    Ok(())
}

fn extract_member(repo_root: &Path, member: &str, graph: &mut Graph) -> Result<()> {
    let manifest_path = repo_root.join(member).join("Cargo.toml");
    let text = std::fs::read_to_string(&manifest_path)
        .context(format!("reading {}", manifest_path.display()))?;
    let manifest: toml::Table =
        toml::from_str(&text).context(format!("parsing {}", manifest_path.display()))?;

    let pkg_name = manifest
        .get("package")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .context(format!("{member}/Cargo.toml has no package.name"))?
        .to_string();

    let dir = Path::new(member)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(member)
        .to_string();
    let short = pkg_name
        .strip_prefix("aeonrift-")
        .unwrap_or(&pkg_name)
        .to_string();

    let source = rel_path(repo_root, &manifest_path);
    let mut node = Node::new(
        EntityKind::Crate,
        &pkg_name,
        Provenance::from(&source, "cargo-parse"),
    )
    .with_prop("dir", &dir)
    .with_prop("short", &short)
    .with_prop("path", member);
    if let Some(desc) = manifest
        .get("package")
        .and_then(|p| p.get("description"))
        .and_then(|d| d.as_str())
    {
        node = node.with_prop("description", desc);
    }
    graph.add_node(node);

    // Workspace CONTAINS Crate.
    graph.add_edge(Edge::new(
        NodeRef::new(EntityKind::Workspace, WORKSPACE_ID),
        RelationKind::Contains,
        NodeRef::new(EntityKind::Crate, &pkg_name),
        Provenance::from(
            rel_path(repo_root, &repo_root.join("Cargo.toml")),
            "cargo-parse",
        ),
    ));

    // Path dependencies -> DEPENDS_ON (Crate -> Crate).
    if let Some(deps) = manifest.get("dependencies").and_then(|d| d.as_table()) {
        for (dep_name, dep_val) in deps {
            let is_path_dep = dep_val
                .as_table()
                .map(|t| t.contains_key("path"))
                .unwrap_or(false);
            if !is_path_dep {
                continue; // registry/git deps (e.g. bevy, serde) are out of scope.
            }
            graph.add_edge(Edge::new(
                NodeRef::new(EntityKind::Crate, &pkg_name),
                RelationKind::DependsOn,
                NodeRef::new(EntityKind::Crate, dep_name.clone()),
                Provenance::from(&source, "cargo-parse"),
            ));
        }
    }
    Ok(())
}

/// Repo-relative display path for stable provenance strings.
fn rel_path(repo_root: &Path, path: &Path) -> String {
    path.strip_prefix(repo_root)
        .unwrap_or(path)
        .to_string_lossy()
        .to_string()
}

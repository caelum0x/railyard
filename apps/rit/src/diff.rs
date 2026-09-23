//! Tree diff: compare two tree snapshots and report the per-path changes.
//!
//! This is the git-domain half of a forge "pull request diff": given two tree
//! object ids (typically the trees of a base and head commit), it returns the
//! set of file paths that were **added**, **deleted**, or **modified**, with the
//! old/new blob ids and modes. Rendering those blobs into a textual patch is a
//! presentation concern left to the caller (the forge). Rename detection is out
//! of scope (a delete + add is reported), matching a conservative first cut.

use crate::error::{ForgeError, Result};
use crate::fsck::ObjectReader;
use crate::object::Object;
use crate::oid::ObjectId;
use std::collections::BTreeMap;

/// Upper bound on files considered in a single diff (a fail-closed guard against
/// pathological trees).
pub const MAX_DIFF_ENTRIES: usize = 1_000_000;
/// Maximum tree recursion depth.
pub const MAX_DIFF_DEPTH: usize = 256;

/// The kind of change to a path between two trees.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    /// Present only in the new tree.
    Added,
    /// Present only in the old tree.
    Deleted,
    /// Present in both with a different blob id or mode.
    Modified,
}

impl ChangeKind {
    /// A short status token (`A`/`D`/`M`), matching `git diff --name-status`.
    pub fn code(&self) -> char {
        match self {
            ChangeKind::Added => 'A',
            ChangeKind::Deleted => 'D',
            ChangeKind::Modified => 'M',
        }
    }
}

/// One changed path between two trees.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeChange {
    /// The full path (raw bytes, `/`-separated) relative to the tree root.
    pub path: Vec<u8>,
    /// What happened to the path.
    pub kind: ChangeKind,
    /// The old mode/oid (absent for an addition).
    pub old_mode: Option<String>,
    pub old_oid: Option<ObjectId>,
    /// The new mode/oid (absent for a deletion).
    pub new_mode: Option<String>,
    pub new_oid: Option<ObjectId>,
}

/// Compute the changes between two (optional) trees. `None` means "empty tree",
/// so a brand-new repository's first commit diffs cleanly as all-additions.
pub fn diff_trees(
    reader: &impl ObjectReader,
    old: Option<ObjectId>,
    new: Option<ObjectId>,
) -> Result<Vec<TreeChange>> {
    let mut old_map = BTreeMap::new();
    if let Some(tree) = old {
        flatten(reader, tree, Vec::new(), 0, &mut old_map)?;
    }
    let mut new_map = BTreeMap::new();
    if let Some(tree) = new {
        flatten(reader, tree, Vec::new(), 0, &mut new_map)?;
    }

    let mut changes = Vec::new();
    for (path, (new_mode, new_oid)) in &new_map {
        match old_map.get(path) {
            None => changes.push(TreeChange {
                path: path.clone(),
                kind: ChangeKind::Added,
                old_mode: None,
                old_oid: None,
                new_mode: Some(new_mode.clone()),
                new_oid: Some(*new_oid),
            }),
            Some((old_mode, old_oid)) if old_oid != new_oid || old_mode != new_mode => changes
                .push(TreeChange {
                    path: path.clone(),
                    kind: ChangeKind::Modified,
                    old_mode: Some(old_mode.clone()),
                    old_oid: Some(*old_oid),
                    new_mode: Some(new_mode.clone()),
                    new_oid: Some(*new_oid),
                }),
            Some(_) => {} // identical — not a change
        }
    }
    for (path, (old_mode, old_oid)) in &old_map {
        if !new_map.contains_key(path) {
            changes.push(TreeChange {
                path: path.clone(),
                kind: ChangeKind::Deleted,
                old_mode: Some(old_mode.clone()),
                old_oid: Some(*old_oid),
                new_mode: None,
                new_oid: None,
            });
        }
    }
    changes.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(changes)
}

/// Recursively flatten a tree into `path -> (mode, blob oid)` leaf entries.
fn flatten(
    reader: &impl ObjectReader,
    tree_oid: ObjectId,
    prefix: Vec<u8>,
    depth: usize,
    out: &mut BTreeMap<Vec<u8>, (String, ObjectId)>,
) -> Result<()> {
    if depth > MAX_DIFF_DEPTH {
        return Err(ForgeError::ResourceLimit(format!(
            "tree depth exceeds {MAX_DIFF_DEPTH}"
        )));
    }
    let Object::Tree(tree) = reader.read_object(&tree_oid)? else {
        return Err(ForgeError::malformed("tree", "diff input is not a tree"));
    };
    for entry in tree.entries {
        let mut path = prefix.clone();
        if !path.is_empty() {
            path.push(b'/');
        }
        path.extend_from_slice(&entry.name);
        if entry.kind() == "tree" {
            flatten(reader, entry.oid, path, depth + 1, out)?;
        } else {
            out.insert(path, (entry.mode.clone(), entry.oid));
            if out.len() > MAX_DIFF_ENTRIES {
                return Err(ForgeError::ResourceLimit(format!(
                    "diff exceeds {MAX_DIFF_ENTRIES} paths"
                )));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::ObjectType;
    use crate::{Repository, Tree, TreeEntry};

    fn blob(repo: &Repository, data: &[u8]) -> ObjectId {
        repo.hash_object(ObjectType::Blob, data, true).unwrap()
    }

    fn tree(repo: &Repository, entries: Vec<TreeEntry>) -> ObjectId {
        let mut t = Tree { entries };
        t.canonicalize();
        repo.hash_object(ObjectType::Tree, &t.serialize(), true)
            .unwrap()
    }

    #[test]
    fn reports_add_modify_delete() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();
        let a1 = blob(&repo, b"one\n");
        let b1 = blob(&repo, b"keep\n");
        let old = tree(
            &repo,
            vec![
                TreeEntry {
                    mode: "100644".into(),
                    name: b"a.txt".to_vec(),
                    oid: a1,
                },
                TreeEntry {
                    mode: "100644".into(),
                    name: b"b.txt".to_vec(),
                    oid: b1,
                },
            ],
        );
        let a2 = blob(&repo, b"two\n");
        let c1 = blob(&repo, b"new\n");
        let new = tree(
            &repo,
            vec![
                TreeEntry {
                    mode: "100644".into(),
                    name: b"a.txt".to_vec(),
                    oid: a2,
                },
                TreeEntry {
                    mode: "100644".into(),
                    name: b"b.txt".to_vec(),
                    oid: b1,
                },
                TreeEntry {
                    mode: "100644".into(),
                    name: b"c.txt".to_vec(),
                    oid: c1,
                },
            ],
        );
        let changes = diff_trees(repo.odb(), Some(old), Some(new)).unwrap();
        assert_eq!(changes.len(), 2);
        assert_eq!(changes[0].path, b"a.txt");
        assert_eq!(changes[0].kind, ChangeKind::Modified);
        assert_eq!(changes[1].path, b"c.txt");
        assert_eq!(changes[1].kind, ChangeKind::Added);

        // Reverse diff sees the deletion.
        let rev = diff_trees(repo.odb(), Some(new), Some(old)).unwrap();
        assert!(rev
            .iter()
            .any(|c| c.path == b"c.txt" && c.kind == ChangeKind::Deleted));
    }
}

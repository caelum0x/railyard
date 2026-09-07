//! Strict, bounded connectivity verification for received Git object graphs.
//!
//! The verifier deliberately operates over an [`ObjectReader`] rather than a
//! concrete repository. Receive quarantine can therefore overlay isolated
//! incoming objects on the existing object database without publishing either
//! source before the complete graph has passed validation.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use crate::error::{ForgeError, Result};
use crate::object::{Object, ObjectType};
use crate::odb::Odb;
use crate::oid::ObjectId;
use crate::tree::TreeEntry;

/// Read-only object source used by fsck and ancestry walks.
pub trait ObjectReader {
    fn read_object(&self, oid: &ObjectId) -> Result<Object>;
}

impl ObjectReader for Odb {
    fn read_object(&self, oid: &ObjectId) -> Result<Object> {
        self.read(oid)
    }
}

/// Defensive graph limits applied before any received ref can move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectivityLimits {
    pub max_objects: usize,
    pub max_depth: usize,
}

impl Default for ConnectivityLimits {
    fn default() -> Self {
        Self {
            max_objects: 1_000_000,
            max_depth: 4096,
        }
    }
}

/// Verified reachability result. Receive uses this to reject disconnected
/// objects rather than allowing an adversary to fill the object database with
/// data unrelated to the proposed ref update.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectivityReport {
    pub reachable: BTreeSet<ObjectId>,
}

/// Verify every object reachable from `roots`, including object-kind edges,
/// strict tree structure, annotated-tag targets, and commit ancestry.
pub fn check_connectivity(
    reader: &impl ObjectReader,
    roots: &[ObjectId],
    limits: ConnectivityLimits,
) -> Result<ConnectivityReport> {
    if roots.is_empty() {
        return Err(ForgeError::Connectivity(
            "at least one graph root is required".into(),
        ));
    }
    let mut reachable = BTreeSet::new();
    let mut object_types = BTreeMap::new();
    let mut pending = roots
        .iter()
        .copied()
        .map(|oid| (oid, None, 0usize))
        .collect::<Vec<_>>();

    while let Some((oid, expected, depth)) = pending.pop() {
        if depth > limits.max_depth {
            return Err(ForgeError::ResourceLimit(format!(
                "object graph exceeds depth {}",
                limits.max_depth
            )));
        }
        if let Some(actual) = object_types.get(&oid) {
            if expected.is_some_and(|expected| expected != *actual) {
                return Err(ForgeError::Connectivity(format!(
                    "object {oid} is referenced as incompatible object types"
                )));
            }
            continue;
        }
        if reachable.len() == limits.max_objects {
            return Err(ForgeError::ResourceLimit(format!(
                "object graph exceeds {} objects",
                limits.max_objects
            )));
        }

        let object = reader.read_object(&oid).map_err(|error| {
            ForgeError::Connectivity(format!("cannot read reachable object {oid}: {error}"))
        })?;
        if let Some(expected) = expected {
            if object.object_type() != expected {
                return Err(ForgeError::Connectivity(format!(
                    "object {oid} has type {}, expected {expected}",
                    object.object_type()
                )));
            }
        }
        object_types.insert(oid, object.object_type());
        reachable.insert(oid);

        match object {
            Object::Blob(_) => {}
            Object::Commit(commit) => {
                pending.push((commit.tree, Some(ObjectType::Tree), depth + 1));
                pending.extend(
                    commit
                        .parents
                        .into_iter()
                        .map(|parent| (parent, Some(ObjectType::Commit), depth + 1)),
                );
            }
            Object::Tree(tree) => {
                validate_tree(&tree.entries, oid)?;
                for entry in tree.entries {
                    match entry.mode.as_str() {
                        "40000" | "040000" => {
                            pending.push((entry.oid, Some(ObjectType::Tree), depth + 1));
                        }
                        "160000" => {
                            // A gitlink names a commit in another repository;
                            // its object is intentionally not required locally.
                        }
                        _ => pending.push((entry.oid, Some(ObjectType::Blob), depth + 1)),
                    }
                }
            }
            Object::Tag(tag) => {
                pending.push((tag.object, Some(tag.object_type), depth + 1));
            }
        }
    }

    Ok(ConnectivityReport { reachable })
}

/// Return whether `ancestor` is reachable through commit-parent edges from
/// `descendant`. Non-commit inputs fail closed instead of being treated as a
/// force update.
pub fn is_ancestor(
    reader: &impl ObjectReader,
    ancestor: ObjectId,
    descendant: ObjectId,
    limits: ConnectivityLimits,
) -> Result<bool> {
    let mut visited = HashSet::new();
    let mut pending = vec![(descendant, 0usize)];
    while let Some((oid, depth)) = pending.pop() {
        if oid == ancestor {
            return Ok(true);
        }
        if !visited.insert(oid) {
            continue;
        }
        if visited.len() > limits.max_objects || depth > limits.max_depth {
            return Err(ForgeError::ResourceLimit(
                "commit ancestry exceeds receive limits".into(),
            ));
        }
        let Object::Commit(commit) = reader.read_object(&oid).map_err(|error| {
            ForgeError::Connectivity(format!("cannot read commit {oid}: {error}"))
        })?
        else {
            return Err(ForgeError::Connectivity(format!(
                "branch tip {oid} is not a commit"
            )));
        };
        pending.extend(commit.parents.into_iter().map(|parent| (parent, depth + 1)));
    }
    Ok(false)
}

fn validate_tree(entries: &[TreeEntry], tree_oid: ObjectId) -> Result<()> {
    let mut previous: Option<Vec<u8>> = None;
    for entry in entries {
        if !matches!(
            entry.mode.as_str(),
            "100644" | "100755" | "120000" | "160000" | "40000" | "040000"
        ) {
            return Err(ForgeError::Connectivity(format!(
                "tree {tree_oid} contains invalid mode {}",
                entry.mode
            )));
        }
        if entry.name.is_empty()
            || entry.name == b"."
            || entry.name == b".."
            || entry.name.contains(&b'/')
            || entry.name.contains(&0)
        {
            return Err(ForgeError::Connectivity(format!(
                "tree {tree_oid} contains an unsafe entry name"
            )));
        }
        let key = tree_sort_key(entry);
        if previous.as_ref().is_some_and(|previous| previous >= &key) {
            return Err(ForgeError::Connectivity(format!(
                "tree {tree_oid} entries are duplicated or not canonically ordered"
            )));
        }
        previous = Some(key);
    }
    Ok(())
}

fn tree_sort_key(entry: &TreeEntry) -> Vec<u8> {
    let mut key = entry.name.clone();
    if matches!(entry.mode.as_str(), "40000" | "040000") {
        key.push(b'/');
    }
    key
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::ObjectType;
    use crate::tree::{Tree, TreeEntry};

    #[test]
    fn rejects_tree_type_confusion_and_unsafe_modes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("objects")).unwrap();
        let odb = Odb::new(dir.path());
        let blob = odb.write(ObjectType::Blob, b"not a tree").unwrap();
        let tree = Tree {
            entries: vec![TreeEntry {
                mode: "40000".into(),
                name: b"sub".to_vec(),
                oid: blob,
            }],
        };
        let root = odb.write(ObjectType::Tree, &tree.serialize()).unwrap();
        assert!(matches!(
            check_connectivity(&odb, &[root], ConnectivityLimits::default()),
            Err(ForgeError::Connectivity(_))
        ));

        let invalid = Tree {
            entries: vec![TreeEntry {
                mode: "100777".into(),
                name: b"file".to_vec(),
                oid: blob,
            }],
        };
        let root = odb.write(ObjectType::Tree, &invalid.serialize()).unwrap();
        assert!(check_connectivity(&odb, &[root], ConnectivityLimits::default()).is_err());
    }

    #[test]
    fn rejects_noncanonical_duplicate_tree_entries() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("objects")).unwrap();
        let odb = Odb::new(dir.path());
        let blob = odb.write(ObjectType::Blob, b"x").unwrap();
        let tree = Tree {
            entries: vec![
                TreeEntry {
                    mode: "100644".into(),
                    name: b"same".to_vec(),
                    oid: blob,
                },
                TreeEntry {
                    mode: "100644".into(),
                    name: b"same".to_vec(),
                    oid: blob,
                },
            ],
        };
        let root = odb.write(ObjectType::Tree, &tree.serialize()).unwrap();
        assert!(check_connectivity(&odb, &[root], ConnectivityLimits::default()).is_err());
    }

    #[test]
    fn rejects_one_object_referenced_as_both_blob_and_tree() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("objects")).unwrap();
        let odb = Odb::new(dir.path());
        let blob = odb.write(ObjectType::Blob, b"same oid").unwrap();
        let tree = Tree {
            entries: vec![
                TreeEntry {
                    mode: "40000".into(),
                    name: b"a-tree".to_vec(),
                    oid: blob,
                },
                TreeEntry {
                    mode: "100644".into(),
                    name: b"b-blob".to_vec(),
                    oid: blob,
                },
            ],
        };
        let root = odb.write(ObjectType::Tree, &tree.serialize()).unwrap();
        assert!(matches!(
            check_connectivity(&odb, &[root], ConnectivityLimits::default()),
            Err(ForgeError::Connectivity(_))
        ));
    }
}

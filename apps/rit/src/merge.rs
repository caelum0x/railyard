//! From-scratch three-way merge — merge base, tree merge, and file-content
//! merge — so the forge no longer needs the installed `git merge-tree` bridge.
//!
//! The algorithm is the classic three-way merge:
//! - [`merge_base`] finds a common ancestor via a bounded reachability walk;
//! - [`merge_trees`] flattens the base/ours/theirs trees to path→(mode,oid) maps
//!   and applies the standard per-path rules (only-one-side-changed takes that
//!   side; both-sides-same takes it; divergent blobs are content-merged);
//! - file content is merged with `diffy`'s diff3 (`merge_bytes`), which returns a
//!   clean buffer or one with conflict markers — we treat the latter as a
//!   conflicted path.
//!
//! The result is either a clean merged **tree oid** or the list of conflicted
//! paths — exactly what a forge needs to decide mergeability and to build a real
//! two-parent merge commit (`Repository::commit_tree`), with no external Git.

use std::collections::{BTreeMap, HashSet, VecDeque};

use crate::error::{ForgeError, Result};
use crate::object::{Object, ObjectType};
use crate::oid::ObjectId;
use crate::repo::Repository;

/// Bound on commits walked while searching for a merge base.
const MAX_MERGE_WALK: usize = 500_000;
/// Bound on tree recursion / entries.
const MAX_MERGE_DEPTH: usize = 256;
const MAX_MERGE_ENTRIES: usize = 1_000_000;

/// The outcome of a three-way merge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeOutcome {
    /// A clean merge producing this tree object id.
    Clean(ObjectId),
    /// Conflicting paths (sorted). No tree is produced.
    Conflict(Vec<String>),
}

/// All lowest common ancestors (merge bases) of two commits.
///
/// Correctness note: the closest common ancestor reached by a naïve breadth-
/// first walk from one side is **not** necessarily a *lowest* common ancestor —
/// BFS can surface an older common ancestor before a newer one that is itself a
/// descendant of it, which would pick a too-old base and cause spurious
/// conflicts or a silently wrong merge. This instead computes the true LCA set:
/// the common ancestors that are not themselves a proper ancestor of any other
/// common ancestor. The set has exactly one element for ordinary (diamond)
/// histories and more than one only for criss-cross histories.
pub fn merge_bases(repo: &Repository, a: ObjectId, b: ObjectId) -> Result<Vec<ObjectId>> {
    let a_ancestors = ancestors(repo, a)?;
    let b_ancestors = ancestors(repo, b)?;
    let common: HashSet<ObjectId> = a_ancestors.intersection(&b_ancestors).copied().collect();
    if common.is_empty() {
        return Ok(Vec::new());
    }

    // `non_maximal` collects every node reachable by walking strictly upward
    // (through parents) from any common node. A common node found in this set is
    // a *proper* ancestor of another common node, so it is not a merge base.
    let mut non_maximal: HashSet<ObjectId> = HashSet::new();
    let mut queue: VecDeque<ObjectId> = VecDeque::new();
    for &oid in &common {
        if let Ok(Object::Commit(commit)) = repo.odb().read(&oid) {
            for parent in commit.parents {
                queue.push_back(parent);
            }
        }
    }
    while let Some(oid) = queue.pop_front() {
        if !non_maximal.insert(oid) {
            continue;
        }
        if non_maximal.len() > MAX_MERGE_WALK {
            return Err(ForgeError::ResourceLimit(
                "merge-base walk exceeds limit".into(),
            ));
        }
        if let Ok(Object::Commit(commit)) = repo.odb().read(&oid) {
            for parent in commit.parents {
                queue.push_back(parent);
            }
        }
    }

    let mut bases: Vec<ObjectId> = common
        .into_iter()
        .filter(|oid| !non_maximal.contains(oid))
        .collect();
    bases.sort(); // deterministic order for callers and tests
    Ok(bases)
}

/// The single merge base of two commits, if any.
///
/// Returns the deterministic first of the [`merge_bases`] set. When more than
/// one merge base exists (a criss-cross history) this still returns one, but
/// merging callers should prefer [`merge_commits`], which fails closed rather
/// than committing to an arbitrary base.
pub fn merge_base(repo: &Repository, a: ObjectId, b: ObjectId) -> Result<Option<ObjectId>> {
    Ok(merge_bases(repo, a, b)?.into_iter().next())
}

/// Three-way merge two commits: merge their trees relative to their merge base.
///
/// Fails closed with [`ForgeError::MergeMultipleBases`] on a criss-cross
/// history (more than one merge base). A single-base three-way merge cannot
/// pick between multiple bases without a recursive/virtual-base strategy (out
/// of scope for this slice), and guessing one risks silently mis-resolving a
/// hunk — so this reports the ambiguity instead of producing a wrong tree.
pub fn merge_commits(repo: &Repository, ours: ObjectId, theirs: ObjectId) -> Result<MergeOutcome> {
    let bases = merge_bases(repo, ours, theirs)?;
    if bases.len() > 1 {
        return Err(ForgeError::MergeMultipleBases(bases.len()));
    }
    let base_tree = match bases.first() {
        Some(oid) => Some(repo.peel_to_commit(*oid)?.tree),
        None => None,
    };
    let ours_tree = repo.peel_to_commit(ours)?.tree;
    let theirs_tree = repo.peel_to_commit(theirs)?.tree;
    merge_trees(repo, base_tree, ours_tree, theirs_tree)
}

/// Three-way merge three trees. `base` is `None` for an empty common ancestor.
pub fn merge_trees(
    repo: &Repository,
    base: Option<ObjectId>,
    ours: ObjectId,
    theirs: ObjectId,
) -> Result<MergeOutcome> {
    let base_map = flatten_opt(repo, base)?;
    let ours_map = flatten(repo, ours)?;
    let theirs_map = flatten(repo, theirs)?;

    // Union of all paths across the three trees.
    let mut paths: Vec<Vec<u8>> = base_map
        .keys()
        .chain(ours_map.keys())
        .chain(theirs_map.keys())
        .cloned()
        .collect();
    paths.sort();
    paths.dedup();

    let mut merged: BTreeMap<Vec<u8>, (String, ObjectId)> = BTreeMap::new();
    let mut conflicts: Vec<String> = Vec::new();

    for path in paths {
        let b = base_map.get(&path);
        let o = ours_map.get(&path);
        let t = theirs_map.get(&path);
        match resolve_path(repo, &path, b, o, t)? {
            PathResolution::Take(Some(entry)) => {
                merged.insert(path, entry);
            }
            PathResolution::Take(None) => { /* deleted on both / took a deletion */ }
            PathResolution::Conflict => {
                conflicts.push(String::from_utf8_lossy(&path).into_owned());
            }
        }
    }

    // A file/directory name collision (a path is also a directory prefix of
    // another) cannot be represented in one tree — treat as a conflict.
    detect_collisions(&merged, &mut conflicts);

    if !conflicts.is_empty() {
        conflicts.sort();
        conflicts.dedup();
        return Ok(MergeOutcome::Conflict(conflicts));
    }
    let tree = build_tree(repo, &merged)?;
    Ok(MergeOutcome::Clean(tree))
}

/// The per-path decision.
enum PathResolution {
    /// Take this entry (or a deletion when `None`).
    Take(Option<(String, ObjectId)>),
    /// The path conflicts.
    Conflict,
}

type Entry = (String, ObjectId);

fn resolve_path(
    repo: &Repository,
    path: &[u8],
    b: Option<&Entry>,
    o: Option<&Entry>,
    t: Option<&Entry>,
) -> Result<PathResolution> {
    // Both sides identical (including both absent) → take either.
    if o == t {
        return Ok(PathResolution::Take(o.cloned()));
    }
    // Ours unchanged vs base → take theirs' change (which may be a deletion).
    if o == b {
        return Ok(PathResolution::Take(t.cloned()));
    }
    // Theirs unchanged vs base → take ours' change.
    if t == b {
        return Ok(PathResolution::Take(o.cloned()));
    }
    // Both sides changed differently.
    match (o, t) {
        // Modify/modify: content-merge the two blobs against the base.
        (Some((omode, ooid)), Some((tmode, toid))) => {
            if omode != tmode {
                return Ok(PathResolution::Conflict); // mode conflict
            }
            let base_bytes = match b {
                Some((_, boid)) => repo.blob(boid)?.data,
                None => Vec::new(), // added on both sides
            };
            let ours_bytes = repo.blob(ooid)?.data;
            let theirs_bytes = repo.blob(toid)?.data;
            // Binary content that diverges cannot be line-merged safely.
            if is_binary(&base_bytes) || is_binary(&ours_bytes) || is_binary(&theirs_bytes) {
                return Ok(PathResolution::Conflict);
            }
            match diffy::merge_bytes(&base_bytes, &ours_bytes, &theirs_bytes) {
                Ok(bytes) => {
                    let oid = repo.hash_object(ObjectType::Blob, &bytes, true)?;
                    Ok(PathResolution::Take(Some((omode.clone(), oid))))
                }
                Err(_) => {
                    let _ = path; // path captured by caller for the conflict list
                    Ok(PathResolution::Conflict)
                }
            }
        }
        // Modify/delete (one side removed, the other changed) → conflict.
        _ => Ok(PathResolution::Conflict),
    }
}

/// A blob is treated as binary if it contains a NUL byte.
fn is_binary(data: &[u8]) -> bool {
    data.contains(&0)
}

/// Flatten an optional tree; `None` → empty map.
fn flatten_opt(repo: &Repository, tree: Option<ObjectId>) -> Result<BTreeMap<Vec<u8>, Entry>> {
    match tree {
        Some(oid) => flatten(repo, oid),
        None => Ok(BTreeMap::new()),
    }
}

/// Flatten a tree to path→(mode,oid) leaf entries.
fn flatten(repo: &Repository, tree: ObjectId) -> Result<BTreeMap<Vec<u8>, Entry>> {
    let mut out = BTreeMap::new();
    flatten_into(repo, tree, Vec::new(), 0, &mut out)?;
    Ok(out)
}

fn flatten_into(
    repo: &Repository,
    tree_oid: ObjectId,
    prefix: Vec<u8>,
    depth: usize,
    out: &mut BTreeMap<Vec<u8>, Entry>,
) -> Result<()> {
    if depth > MAX_MERGE_DEPTH {
        return Err(ForgeError::ResourceLimit(format!(
            "merge tree depth exceeds {MAX_MERGE_DEPTH}"
        )));
    }
    for entry in repo.tree(&tree_oid)?.entries {
        let mut path = prefix.clone();
        if !path.is_empty() {
            path.push(b'/');
        }
        path.extend_from_slice(&entry.name);
        if entry.kind() == "tree" {
            flatten_into(repo, entry.oid, path, depth + 1, out)?;
        } else {
            out.insert(path, (entry.mode.clone(), entry.oid));
            if out.len() > MAX_MERGE_ENTRIES {
                return Err(ForgeError::ResourceLimit(format!(
                    "merge exceeds {MAX_MERGE_ENTRIES} paths"
                )));
            }
        }
    }
    Ok(())
}

/// Flag any path that is also a directory prefix of another path.
fn detect_collisions(merged: &BTreeMap<Vec<u8>, Entry>, conflicts: &mut Vec<String>) {
    let paths: Vec<&Vec<u8>> = merged.keys().collect();
    for (i, p) in paths.iter().enumerate() {
        let mut prefix = (*p).clone();
        prefix.push(b'/');
        // Any other path starting with `p/` means `p` is both file and dir.
        for q in paths.iter().skip(i + 1) {
            if q.starts_with(&prefix) {
                conflicts.push(String::from_utf8_lossy(p).into_owned());
                break;
            }
        }
    }
}

// --- tree reconstruction ----------------------------------------------------

#[derive(Default)]
struct TreeNode {
    files: BTreeMap<Vec<u8>, Entry>,
    dirs: BTreeMap<Vec<u8>, TreeNode>,
}

impl TreeNode {
    fn insert(&mut self, path: &[u8], entry: Entry) {
        let mut parts = path.split(|b| *b == b'/').peekable();
        let mut node = self;
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                node.files.insert(part.to_vec(), entry);
                return;
            }
            node = node.dirs.entry(part.to_vec()).or_default();
        }
    }

    fn write(&self, repo: &Repository) -> Result<ObjectId> {
        use crate::tree::{Tree, TreeEntry};
        let mut entries = Vec::with_capacity(self.files.len() + self.dirs.len());
        for (name, (mode, oid)) in &self.files {
            entries.push(TreeEntry {
                mode: mode.clone(),
                name: name.clone(),
                oid: *oid,
            });
        }
        for (name, child) in &self.dirs {
            entries.push(TreeEntry {
                mode: "40000".into(),
                name: name.clone(),
                oid: child.write(repo)?,
            });
        }
        let mut tree = Tree { entries };
        tree.canonicalize();
        repo.hash_object(ObjectType::Tree, &tree.serialize(), true)
    }
}

fn build_tree(repo: &Repository, merged: &BTreeMap<Vec<u8>, Entry>) -> Result<ObjectId> {
    let mut root = TreeNode::default();
    for (path, entry) in merged {
        root.insert(path, entry.clone());
    }
    root.write(repo)
}

/// All ancestors of `start` (inclusive), bounded.
fn ancestors(repo: &Repository, start: ObjectId) -> Result<HashSet<ObjectId>> {
    let mut set = HashSet::new();
    let mut queue = VecDeque::from([start]);
    while let Some(oid) = queue.pop_front() {
        if !set.insert(oid) {
            continue;
        }
        if set.len() > MAX_MERGE_WALK {
            return Err(ForgeError::ResourceLimit(
                "merge-base walk exceeds limit".into(),
            ));
        }
        if let Ok(Object::Commit(commit)) = repo.odb().read(&oid) {
            for parent in commit.parents {
                queue.push_back(parent);
            }
        }
    }
    Ok(set)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Signature, Tree, TreeEntry};

    fn sig() -> Signature {
        Signature::parse("T <t@e.com> 1700000000 +0000", "commit").unwrap()
    }

    fn blob(repo: &Repository, data: &[u8]) -> ObjectId {
        repo.hash_object(ObjectType::Blob, data, true).unwrap()
    }

    fn tree(repo: &Repository, entries: Vec<(&str, ObjectId)>) -> ObjectId {
        let mut t = Tree {
            entries: entries
                .into_iter()
                .map(|(name, oid)| TreeEntry {
                    mode: "100644".into(),
                    name: name.as_bytes().to_vec(),
                    oid,
                })
                .collect(),
        };
        t.canonicalize();
        repo.hash_object(ObjectType::Tree, &t.serialize(), true)
            .unwrap()
    }

    fn tree_modes(repo: &Repository, entries: Vec<(&str, &str, ObjectId)>) -> ObjectId {
        let mut t = Tree {
            entries: entries
                .into_iter()
                .map(|(name, mode, oid)| TreeEntry {
                    mode: mode.into(),
                    name: name.as_bytes().to_vec(),
                    oid,
                })
                .collect(),
        };
        t.canonicalize();
        repo.hash_object(ObjectType::Tree, &t.serialize(), true)
            .unwrap()
    }

    fn commit(repo: &Repository, tree: ObjectId, parents: Vec<ObjectId>) -> ObjectId {
        repo.commit_tree(tree, parents, sig(), sig(), b"c\n", None)
            .unwrap()
    }

    #[test]
    fn clean_merge_of_divergent_changes() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();
        // base: a="1\n2\n3\n", shared="x\n"
        let a0 = blob(&repo, b"1\n2\n3\n");
        let shared = blob(&repo, b"x\n");
        let base_tree = tree(&repo, vec![("a.txt", a0), ("shared.txt", shared)]);
        let base = commit(&repo, base_tree, vec![]);
        // ours: change line 1 + add ours.txt
        let a_ours = blob(&repo, b"ONE\n2\n3\n");
        let ours_only = blob(&repo, b"ours\n");
        let ours_tree = tree(
            &repo,
            vec![
                ("a.txt", a_ours),
                ("shared.txt", shared),
                ("ours.txt", ours_only),
            ],
        );
        let ours = commit(&repo, ours_tree, vec![base]);
        // theirs: change line 3 + add theirs.txt
        let a_theirs = blob(&repo, b"1\n2\nTHREE\n");
        let theirs_only = blob(&repo, b"theirs\n");
        let theirs_tree = tree(
            &repo,
            vec![
                ("a.txt", a_theirs),
                ("shared.txt", shared),
                ("theirs.txt", theirs_only),
            ],
        );
        let theirs = commit(&repo, theirs_tree, vec![base]);

        match merge_commits(&repo, ours, theirs).unwrap() {
            MergeOutcome::Clean(merged_tree) => {
                // a.txt must contain BOTH edits; both added files present.
                let flat = flatten(&repo, merged_tree).unwrap();
                let a = repo.blob(&flat[b"a.txt".as_slice()].1).unwrap().data;
                assert_eq!(a, b"ONE\n2\nTHREE\n");
                assert!(flat.contains_key(b"ours.txt".as_slice()));
                assert!(flat.contains_key(b"theirs.txt".as_slice()));
            }
            other => panic!("expected clean merge, got {other:?}"),
        }
    }

    #[test]
    fn conflicting_edits_report_conflict() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();
        let a0 = blob(&repo, b"line\n");
        let base_tree = tree(&repo, vec![("a.txt", a0)]);
        let base = commit(&repo, base_tree, vec![]);
        let a_ours = blob(&repo, b"ours change\n");
        let ours = commit(&repo, tree(&repo, vec![("a.txt", a_ours)]), vec![base]);
        let a_theirs = blob(&repo, b"theirs change\n");
        let theirs = commit(&repo, tree(&repo, vec![("a.txt", a_theirs)]), vec![base]);

        match merge_commits(&repo, ours, theirs).unwrap() {
            MergeOutcome::Conflict(paths) => assert_eq!(paths, vec!["a.txt".to_string()]),
            other => panic!("expected conflict, got {other:?}"),
        }
    }

    // merge_base must return the *lowest* common ancestor, not merely the first
    // common ancestor a breadth-first walk happens to reach. In a linear
    // base->g1->g2 history where both sides fork at g2, the only merge base is
    // g2 — never g1 or the root.
    #[test]
    fn merge_base_is_the_lowest_common_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();
        let blob0 = blob(&repo, b"0\n");
        let root = commit(&repo, tree(&repo, vec![("a.txt", blob0)]), vec![]);
        let g1 = commit(&repo, tree(&repo, vec![("a.txt", blob0)]), vec![root]);
        let g2 = commit(&repo, tree(&repo, vec![("a.txt", blob0)]), vec![g1]);
        // Distinct trees so the two tips are distinct commits (not the same oid).
        let ours = commit(
            &repo,
            tree(&repo, vec![("a.txt", blob(&repo, b"ours\n"))]),
            vec![g2],
        );
        let theirs = commit(
            &repo,
            tree(&repo, vec![("a.txt", blob(&repo, b"theirs\n"))]),
            vec![g2],
        );

        assert_eq!(merge_bases(&repo, ours, theirs).unwrap(), vec![g2]);
        assert_eq!(merge_base(&repo, ours, theirs).unwrap(), Some(g2));
        // No common history → no merge base.
        let orphan = commit(&repo, tree(&repo, vec![("z.txt", blob0)]), vec![]);
        assert_eq!(merge_bases(&repo, ours, orphan).unwrap(), Vec::new());
        assert_eq!(merge_base(&repo, ours, orphan).unwrap(), None);
    }

    // A criss-cross history has two lowest common ancestors. merge_bases must
    // report both, and merge_commits must fail closed rather than silently
    // pick one and risk mis-resolving content.
    #[test]
    fn criss_cross_history_reports_both_bases_and_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();
        let blob0 = blob(&repo, b"0\n");
        let root = commit(&repo, tree(&repo, vec![("a.txt", blob0)]), vec![]);
        let a = commit(
            &repo,
            tree(&repo, vec![("a.txt", blob(&repo, b"a\n"))]),
            vec![root],
        );
        let b = commit(
            &repo,
            tree(&repo, vec![("a.txt", blob(&repo, b"b\n"))]),
            vec![root],
        );
        // Two independent merges of the same pair of branches => criss-cross.
        let m1 = commit(
            &repo,
            tree(&repo, vec![("a.txt", blob(&repo, b"m1\n"))]),
            vec![a, b],
        );
        let m2 = commit(
            &repo,
            tree(&repo, vec![("a.txt", blob(&repo, b"m2\n"))]),
            vec![a, b],
        );

        let mut bases = merge_bases(&repo, m1, m2).unwrap();
        bases.sort();
        let mut expected = vec![a, b];
        expected.sort();
        assert_eq!(bases, expected, "both a and b are lowest common ancestors");

        assert!(matches!(
            merge_commits(&repo, m1, m2),
            Err(ForgeError::MergeMultipleBases(2))
        ));
    }

    // A path modified on one side and deleted on the other is a conflict.
    #[test]
    fn modify_delete_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();
        let a0 = blob(&repo, b"base\n");
        let keep = blob(&repo, b"keep\n");
        let base = commit(
            &repo,
            tree(&repo, vec![("a.txt", a0), ("k.txt", keep)]),
            vec![],
        );
        // ours modifies a.txt; theirs deletes a.txt (tree omits it).
        let a_mod = blob(&repo, b"changed\n");
        let ours = commit(
            &repo,
            tree(&repo, vec![("a.txt", a_mod), ("k.txt", keep)]),
            vec![base],
        );
        let theirs = commit(&repo, tree(&repo, vec![("k.txt", keep)]), vec![base]);

        match merge_commits(&repo, ours, theirs).unwrap() {
            MergeOutcome::Conflict(paths) => assert_eq!(paths, vec!["a.txt".to_string()]),
            other => panic!("expected modify/delete conflict, got {other:?}"),
        }
    }

    // The same path changing mode on one side and content on the other is a
    // conflict (a mode change cannot be silently reconciled with a content edit).
    #[test]
    fn mode_change_conflicts_with_content_change() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();
        let content = blob(&repo, b"x\n");
        let base = commit(
            &repo,
            tree_modes(&repo, vec![("s.sh", "100644", content)]),
            vec![],
        );
        // ours: chmod +x, same content. theirs: same mode, new content.
        let ours = commit(
            &repo,
            tree_modes(&repo, vec![("s.sh", "100755", content)]),
            vec![base],
        );
        let new_content = blob(&repo, b"y\n");
        let theirs = commit(
            &repo,
            tree_modes(&repo, vec![("s.sh", "100644", new_content)]),
            vec![base],
        );

        match merge_commits(&repo, ours, theirs).unwrap() {
            MergeOutcome::Conflict(paths) => assert_eq!(paths, vec!["s.sh".to_string()]),
            other => panic!("expected mode conflict, got {other:?}"),
        }
    }

    // The same new file added identically on both sides merges cleanly.
    #[test]
    fn add_add_identical_is_clean() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();
        let base = commit(&repo, tree(&repo, vec![]), vec![]);
        let same = blob(&repo, b"new file\n");
        // Each side also adds a unique file so the two tips are distinct commits;
        // new.txt is the add/add-identical path under test.
        let ours_only = blob(&repo, b"o\n");
        let theirs_only = blob(&repo, b"t\n");
        let ours = commit(
            &repo,
            tree(&repo, vec![("new.txt", same), ("o.txt", ours_only)]),
            vec![base],
        );
        let theirs = commit(
            &repo,
            tree(&repo, vec![("new.txt", same), ("t.txt", theirs_only)]),
            vec![base],
        );

        match merge_commits(&repo, ours, theirs).unwrap() {
            MergeOutcome::Clean(t) => {
                let flat = flatten(&repo, t).unwrap();
                assert_eq!(flat[b"new.txt".as_slice()].1, same);
                assert!(flat.contains_key(b"o.txt".as_slice()));
                assert!(flat.contains_key(b"t.txt".as_slice()));
            }
            other => panic!("expected clean merge, got {other:?}"),
        }
    }

    // Divergent binary content cannot be line-merged and must conflict rather
    // than corrupt the file.
    #[test]
    fn divergent_binary_conflicts() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();
        let base_bin = blob(&repo, b"\x00base");
        let base = commit(&repo, tree(&repo, vec![("a.bin", base_bin)]), vec![]);
        let ours = commit(
            &repo,
            tree(&repo, vec![("a.bin", blob(&repo, b"\x00ours"))]),
            vec![base],
        );
        let theirs = commit(
            &repo,
            tree(&repo, vec![("a.bin", blob(&repo, b"\x00theirs"))]),
            vec![base],
        );

        match merge_commits(&repo, ours, theirs).unwrap() {
            MergeOutcome::Conflict(paths) => assert_eq!(paths, vec!["a.bin".to_string()]),
            other => panic!("expected binary conflict, got {other:?}"),
        }
    }
}

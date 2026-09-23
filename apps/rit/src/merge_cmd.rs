//! `git merge` porcelain — wire the from-scratch three-way [`crate::merge`]
//! engine into a real command that produces a two-parent commit or surfaces
//! conflicts (conflict-marker worktree + unmerged index + `MERGE_HEAD`).
//!
//! This module also owns the **shared history-editing primitives** that the
//! [`crate::sequencer`] (rebase / cherry-pick) and [`crate::stash`] modules
//! build on:
//!
//! - [`build_worktree_merge`] — a three-way tree merge that, unlike
//!   [`crate::merge::merge_trees`], also returns the conflict-marker content and
//!   the stage 1/2/3 index entries needed to *represent* a conflict on disk;
//! - [`install_unmerged`] — install those unmerged stages into `.git/index`;
//! - [`force_reset`] — discard worktree/index state back to a target tree
//!   (used by `--abort` / `--skip` where the safe [`Repository::checkout_tree`]
//!   would refuse because of conflict-marker "local changes");
//! - small `.git`-directory state-file helpers (`MERGE_HEAD`, `ORIG_HEAD`, …).
//!
//! The CLI dispatch (clap wiring) is intentionally **not** here — the integrator
//! calls [`cmd_merge`], [`continue_merge`], and [`abort_merge`] from `main.rs`.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::{ForgeError, Result};
use crate::fsck::{is_ancestor as fsck_is_ancestor, ConnectivityLimits};
use crate::index::{Index, IndexEntry};
use crate::object::ObjectType;
use crate::oid::ObjectId;
use crate::refs::RefTarget;
use crate::repo::Repository;
use crate::signature::Signature;
use crate::tree::{Tree, TreeEntry};

/// Bound on tree recursion while flattening for a merge.
const MAX_MERGE_DEPTH: usize = 256;

// ── public command surface ──────────────────────────────────────────────────

/// How a completed [`cmd_merge`] resolved.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MergeKind {
    /// `theirs` was already reachable from `HEAD`; nothing to do.
    AlreadyUpToDate,
    /// `HEAD` was an ancestor of `theirs`; the branch was fast-forwarded.
    FastForward(ObjectId),
    /// A real three-way merge produced this two-parent commit.
    Merged(ObjectId),
    /// The merge conflicted on these paths; `MERGE_HEAD` was recorded and the
    /// worktree now contains conflict markers.
    Conflicts(Vec<String>),
}

/// Outcome of a [`cmd_merge`] invocation.
#[derive(Clone, Debug)]
pub struct MergeResult {
    /// The resolution.
    pub kind: MergeKind,
    /// The other side's commit that was merged (the `theirs` tip).
    pub theirs: ObjectId,
}

/// `git merge <target>` into the current branch.
///
/// Resolves the merge base, then:
/// - returns [`MergeKind::AlreadyUpToDate`] when `target` is reachable from HEAD;
/// - fast-forwards (unless `no_ff`) when HEAD is an ancestor of `target`;
/// - otherwise performs a three-way merge, writing a two-parent commit on a
///   clean merge or recording `MERGE_HEAD` + conflict markers on a conflict.
///
/// Fails closed with [`ForgeError::MergeMultipleBases`] on a criss-cross
/// history, matching the single-base scope of [`crate::merge`].
pub fn cmd_merge(
    repo: &Repository,
    target: &str,
    sig: &Signature,
    message: Option<&str>,
    no_ff: bool,
) -> Result<MergeResult> {
    let git_dir = repo.git_dir();
    let head_ref = head_branch(repo)?;
    let ours = repo.head().map_err(|_| {
        ForgeError::InvalidRef("cannot merge into an empty repository (no HEAD commit)".into())
    })?;
    let theirs = repo.resolve(target)?;
    // Peel a tag/target to its commit so a two-parent merge always links commits.
    let theirs = peel_commit_oid(repo, theirs)?;

    // ORIG_HEAD is Git's "where I was before this operation" anchor.
    write_gitdir_file(git_dir, "ORIG_HEAD", format!("{ours}\n").as_bytes())?;

    if ours == theirs || is_ancestor(repo, theirs, ours)? {
        return Ok(MergeResult {
            kind: MergeKind::AlreadyUpToDate,
            theirs,
        });
    }

    if !no_ff && is_ancestor(repo, ours, theirs)? {
        let tree = repo.peel_to_commit(theirs)?.tree;
        repo.checkout_tree(tree)?;
        repo.refs()
            .compare_and_swap(&head_ref, Some(ours), theirs)?;
        let msg = format!("merge {target}: Fast-forward");
        let _ = repo.refs().append_reflog(
            &head_ref,
            Some(ours),
            theirs,
            &sig.name,
            &sig.email,
            sig.time,
            &sig.tz,
            &msg,
        );
        return Ok(MergeResult {
            kind: MergeKind::FastForward(theirs),
            theirs,
        });
    }

    // True three-way merge.
    let bases = crate::merge::merge_bases(repo, ours, theirs)?;
    if bases.len() > 1 {
        return Err(ForgeError::MergeMultipleBases(bases.len()));
    }
    let base_tree = match bases.first() {
        Some(oid) => Some(repo.peel_to_commit(*oid)?.tree),
        None => None,
    };
    let ours_tree = repo.peel_to_commit(ours)?.tree;
    let theirs_tree = repo.peel_to_commit(theirs)?.tree;

    let message = normalize_message(message, || default_merge_msg(repo, target));

    match build_worktree_merge(repo, base_tree, ours_tree, theirs_tree)? {
        WorktreeMerge::Clean(tree) => {
            repo.checkout_tree(tree)?;
            let new = repo.commit_tree(
                tree,
                vec![ours, theirs],
                sig.clone(),
                sig.clone(),
                message.as_bytes(),
                Some(&head_ref),
            )?;
            Ok(MergeResult {
                kind: MergeKind::Merged(new),
                theirs,
            })
        }
        WorktreeMerge::Conflict {
            tree,
            conflicts,
            stages,
        } => {
            repo.checkout_tree(tree)?;
            install_unmerged(git_dir, &stages)?;
            write_gitdir_file(git_dir, "MERGE_HEAD", format!("{theirs}\n").as_bytes())?;
            write_gitdir_file(git_dir, "MERGE_MSG", message.as_bytes())?;
            Ok(MergeResult {
                kind: MergeKind::Conflicts(conflicts),
                theirs,
            })
        }
    }
}

/// `git merge --continue` / `git commit` after resolving a conflicted merge.
///
/// Requires a resolved (stage-0) index and a recorded `MERGE_HEAD`. Creates the
/// two-parent merge commit from `ORIG_HEAD` (ours) and `MERGE_HEAD` (theirs),
/// advances the branch, and clears the merge state files.
pub fn continue_merge(
    repo: &Repository,
    sig: &Signature,
    message: Option<&str>,
) -> Result<ObjectId> {
    let git_dir = repo.git_dir();
    let theirs = read_gitdir_oid(git_dir, "MERGE_HEAD")
        .ok_or_else(|| ForgeError::InvalidRef("no MERGE_HEAD: not in a conflicted merge".into()))?;
    let ours = repo.head()?;
    let head_ref = head_branch(repo)?;
    // write_tree fails closed if any path is still at an unmerged stage.
    let tree = repo.write_tree()?;
    let stored = read_gitdir_string(git_dir, "MERGE_MSG");
    let message = normalize_message(message.or(stored.as_deref()), || {
        format!("Merge commit '{theirs}'\n")
    });
    let new = repo.commit_tree(
        tree,
        vec![ours, theirs],
        sig.clone(),
        sig.clone(),
        message.as_bytes(),
        Some(&head_ref),
    )?;
    remove_gitdir_file(git_dir, "MERGE_HEAD");
    remove_gitdir_file(git_dir, "MERGE_MSG");
    Ok(new)
}

/// `git merge --abort`: discard a conflicted merge, restoring `ORIG_HEAD`.
pub fn abort_merge(repo: &Repository) -> Result<()> {
    let git_dir = repo.git_dir();
    let orig = read_gitdir_oid(git_dir, "ORIG_HEAD")
        .ok_or_else(|| ForgeError::InvalidRef("no ORIG_HEAD to abort to".into()))?;
    let head_ref = head_branch(repo)?;
    let current = repo.head()?;
    repo.refs()
        .compare_and_swap(&head_ref, Some(current), orig)?;
    force_reset(repo, repo.peel_to_commit(orig)?.tree)?;
    remove_gitdir_file(git_dir, "MERGE_HEAD");
    remove_gitdir_file(git_dir, "MERGE_MSG");
    Ok(())
}

// ── shared three-way merge (content + stages) ───────────────────────────────

/// One unmerged index entry (a stage-1/2/3 slot for a conflicted path).
#[derive(Clone, Debug)]
pub(crate) struct StageEntry {
    pub path: Vec<u8>,
    pub stage: u8,
    pub mode: String,
    pub oid: ObjectId,
}

/// The result of a three-way tree merge that carries enough detail to
/// materialize a conflict on disk.
pub(crate) enum WorktreeMerge {
    /// A clean merge producing this tree (all entries stage 0).
    Clean(ObjectId),
    /// A conflicting merge. `tree` is materializable into the worktree (clean
    /// paths + conflict-marker/ours blobs for conflicted paths); `stages` are
    /// the unmerged index entries to install after checkout.
    Conflict {
        tree: ObjectId,
        conflicts: Vec<String>,
        stages: Vec<StageEntry>,
    },
}

/// Three-way merge three trees, producing merged content **and** conflict
/// representation. `base` is `None` for an empty/unrelated common ancestor.
pub(crate) fn build_worktree_merge(
    repo: &Repository,
    base: Option<ObjectId>,
    ours: ObjectId,
    theirs: ObjectId,
) -> Result<WorktreeMerge> {
    let base_map = match base {
        Some(oid) => flatten_tree(repo, oid)?,
        None => BTreeMap::new(),
    };
    let ours_map = flatten_tree(repo, ours)?;
    let theirs_map = flatten_tree(repo, theirs)?;

    let mut paths: Vec<Vec<u8>> = base_map
        .keys()
        .chain(ours_map.keys())
        .chain(theirs_map.keys())
        .cloned()
        .collect();
    paths.sort();
    paths.dedup();

    let mut result: BTreeMap<Vec<u8>, Entry> = BTreeMap::new();
    let mut conflicts: Vec<String> = Vec::new();
    let mut stages: Vec<StageEntry> = Vec::new();

    for path in paths {
        let b = base_map.get(&path);
        let o = ours_map.get(&path);
        let t = theirs_map.get(&path);

        // Both sides identical (including both absent) → take either.
        if o == t {
            if let Some(entry) = o {
                result.insert(path, entry.clone());
            }
            continue;
        }
        // Ours unchanged vs base → take theirs' change (possibly a deletion).
        if o == b {
            if let Some(entry) = t {
                result.insert(path, entry.clone());
            }
            continue;
        }
        // Theirs unchanged vs base → take ours' change.
        if t == b {
            if let Some(entry) = o {
                result.insert(path, entry.clone());
            }
            continue;
        }

        // Genuine divergence: resolve or record a conflict.
        match (o, t) {
            (Some((omode, ooid)), Some((tmode, toid))) => {
                let base_bytes = match b {
                    Some((_, boid)) => repo.blob(boid)?.data,
                    None => Vec::new(),
                };
                let ours_bytes = repo.blob(ooid)?.data;
                let theirs_bytes = repo.blob(toid)?.data;

                let text_mergeable = omode == tmode
                    && !is_binary(&base_bytes)
                    && !is_binary(&ours_bytes)
                    && !is_binary(&theirs_bytes);
                let worktree = if text_mergeable {
                    match diffy::merge_bytes(&base_bytes, &ours_bytes, &theirs_bytes) {
                        Ok(bytes) => {
                            let oid = repo.hash_object(ObjectType::Blob, &bytes, true)?;
                            result.insert(path.clone(), (omode.clone(), oid));
                            continue;
                        }
                        // Err carries the diff3 output *with* conflict markers.
                        Err(markers) => {
                            let oid = repo.hash_object(ObjectType::Blob, &markers, true)?;
                            (omode.clone(), oid)
                        }
                    }
                } else {
                    // Binary or mode conflict: keep ours in the worktree.
                    (omode.clone(), *ooid)
                };
                result.insert(path.clone(), worktree);
                if let Some((bmode, boid)) = b {
                    stages.push(stage_entry(&path, 1, bmode, *boid));
                }
                stages.push(stage_entry(&path, 2, omode, *ooid));
                stages.push(stage_entry(&path, 3, tmode, *toid));
                conflicts.push(display(&path));
            }
            // Modify/delete: ours changed, theirs removed.
            (Some((omode, ooid)), None) => {
                result.insert(path.clone(), (omode.clone(), *ooid));
                if let Some((bmode, boid)) = b {
                    stages.push(stage_entry(&path, 1, bmode, *boid));
                }
                stages.push(stage_entry(&path, 2, omode, *ooid));
                conflicts.push(display(&path));
            }
            // Delete/modify: ours removed, theirs changed.
            (None, Some((tmode, toid))) => {
                result.insert(path.clone(), (tmode.clone(), *toid));
                if let Some((bmode, boid)) = b {
                    stages.push(stage_entry(&path, 1, bmode, *boid));
                }
                stages.push(stage_entry(&path, 3, tmode, *toid));
                conflicts.push(display(&path));
            }
            // o == t already handled the (None, None) case.
            (None, None) => unreachable!("o != t guarantees at least one side present"),
        }
    }

    let tree = write_tree_from_entries(repo, &result)?;
    if conflicts.is_empty() {
        Ok(WorktreeMerge::Clean(tree))
    } else {
        conflicts.sort();
        conflicts.dedup();
        Ok(WorktreeMerge::Conflict {
            tree,
            conflicts,
            stages,
        })
    }
}

/// Install unmerged (stage 1/2/3) entries for conflicted paths into
/// `.git/index`, replacing whatever stage-0 entry a prior checkout left.
pub(crate) fn install_unmerged(git_dir: &Path, stages: &[StageEntry]) -> Result<()> {
    if stages.is_empty() {
        return Ok(());
    }
    let conflicted: BTreeSet<Vec<u8>> = stages.iter().map(|s| s.path.clone()).collect();
    Index::mutate(git_dir, |index| {
        index
            .entries
            .retain(|entry| !conflicted.contains(&entry.path));
        for stage in stages {
            let mode = u32::from_str_radix(&stage.mode, 8).unwrap_or(0o100644);
            let flags =
                (((stage.stage & 0x3) as u16) << 12) | (stage.path.len().min(0x0fff) as u16);
            index.entries.push(IndexEntry {
                mode,
                file_size: 0,
                oid: stage.oid,
                flags,
                path: stage.path.clone(),
            });
        }
        index
            .entries
            .sort_by(|a, b| a.path.cmp(&b.path).then(a.stage().cmp(&b.stage())));
        Ok(())
    })
}

// ── reset / state-file primitives (shared) ──────────────────────────────────

/// Discard worktree + index state back to `tree`, unconditionally.
///
/// [`Repository::checkout_tree`] refuses to overwrite "locally modified" paths —
/// but after a conflict the worktree *is* full of intentional local changes
/// (conflict markers). This first restages the worktree so those changes are no
/// longer seen as modifications, then applies `tree`. Used by `--abort` /
/// `--skip` and `stash` reset.
pub(crate) fn force_reset(repo: &Repository, tree: ObjectId) -> Result<()> {
    // Restage everything so tracked worktree state == index (no "local changes").
    repo.add(&[PathBuf::from(".")])?;
    repo.checkout_tree(tree)?;
    Ok(())
}

/// The full ref name HEAD symbolically points at, or an error on detached HEAD.
pub(crate) fn head_branch(repo: &Repository) -> Result<String> {
    match repo.refs().read_head()? {
        RefTarget::Symbolic(name) => Ok(name),
        RefTarget::Direct(_) => Err(ForgeError::InvalidRef(
            "HEAD is detached; this operation requires being on a branch".into(),
        )),
    }
}

/// Write a small `.git/<rel>` state file (fsynced), creating parent dirs.
pub(crate) fn write_gitdir_file(git_dir: &Path, rel: &str, content: &[u8]) -> Result<()> {
    let path = git_dir.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::File::create(&path)?;
    file.write_all(content)?;
    file.sync_all()?;
    Ok(())
}

/// Remove a `.git/<rel>` state file if present (absence is not an error).
pub(crate) fn remove_gitdir_file(git_dir: &Path, rel: &str) {
    let _ = std::fs::remove_file(git_dir.join(rel));
}

/// Read a `.git/<rel>` file as a single object id, if present and valid.
pub(crate) fn read_gitdir_oid(git_dir: &Path, rel: &str) -> Option<ObjectId> {
    let raw = std::fs::read_to_string(git_dir.join(rel)).ok()?;
    ObjectId::from_hex(raw.trim()).ok()
}

/// Read a `.git/<rel>` file as a UTF-8 string, if present.
pub(crate) fn read_gitdir_string(git_dir: &Path, rel: &str) -> Option<String> {
    std::fs::read_to_string(git_dir.join(rel)).ok()
}

/// Peel a tag/target oid to the underlying commit oid (bounded).
pub(crate) fn peel_commit_oid(repo: &Repository, oid: ObjectId) -> Result<ObjectId> {
    use crate::object::Object;
    let mut current = oid;
    for _ in 0..10 {
        match repo.read_object(&current)? {
            Object::Commit(_) => return Ok(current),
            Object::Tag(tag) => current = tag.object,
            other => {
                return Err(ForgeError::malformed(
                    "object",
                    format!("{current} is a {}, expected a commit", other.object_type()),
                ))
            }
        }
    }
    Err(ForgeError::InvalidRef(format!(
        "{oid} does not peel to a commit"
    )))
}

// ── internal helpers ────────────────────────────────────────────────────────

type Entry = (String, ObjectId);

fn is_ancestor(repo: &Repository, ancestor: ObjectId, descendant: ObjectId) -> Result<bool> {
    fsck_is_ancestor(
        repo.odb(),
        ancestor,
        descendant,
        ConnectivityLimits::default(),
    )
}

fn is_binary(data: &[u8]) -> bool {
    data.contains(&0)
}

fn display(path: &[u8]) -> String {
    String::from_utf8_lossy(path).into_owned()
}

fn stage_entry(path: &[u8], stage: u8, mode: &str, oid: ObjectId) -> StageEntry {
    StageEntry {
        path: path.to_vec(),
        stage,
        mode: mode.to_string(),
        oid,
    }
}

fn normalize_message(message: Option<&str>, default: impl FnOnce() -> String) -> String {
    match message {
        Some(msg) if msg.ends_with('\n') => msg.to_string(),
        Some(msg) => format!("{msg}\n"),
        None => default(),
    }
}

fn default_merge_msg(repo: &Repository, target: &str) -> String {
    if repo.refs().resolve(&format!("refs/heads/{target}")).is_ok() {
        format!("Merge branch '{target}'\n")
    } else {
        format!("Merge commit '{target}'\n")
    }
}

/// Flatten a tree to path → (mode, oid) leaf entries (bounded recursion).
pub(crate) fn flatten_tree(repo: &Repository, tree: ObjectId) -> Result<BTreeMap<Vec<u8>, Entry>> {
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
        }
    }
    Ok(())
}

/// Reconstruct a canonical Git tree from flattened path → (mode, oid) entries.
pub(crate) fn write_tree_from_entries(
    repo: &Repository,
    entries: &BTreeMap<Vec<u8>, Entry>,
) -> Result<ObjectId> {
    let mut root = MergeTreeNode::default();
    for (path, (mode, oid)) in entries {
        root.insert(path, mode.clone(), *oid);
    }
    root.write(repo)
}

#[derive(Default)]
struct MergeTreeNode {
    files: BTreeMap<Vec<u8>, Entry>,
    dirs: BTreeMap<Vec<u8>, MergeTreeNode>,
}

impl MergeTreeNode {
    fn insert(&mut self, path: &[u8], mode: String, oid: ObjectId) {
        let mut parts = path.split(|b| *b == b'/').peekable();
        let mut node = self;
        while let Some(part) = parts.next() {
            if parts.peek().is_none() {
                // File/dir collision: a directory already claims this name; the
                // directory wins (a single tree cannot hold both). Rare edge —
                // three-way merge marks such paths as conflicts anyway.
                if node.dirs.contains_key(part) {
                    return;
                }
                node.files.insert(part.to_vec(), (mode, oid));
                return;
            }
            // A file previously claimed this name but a deeper path needs a
            // directory here; the directory wins.
            node.files.remove(part);
            node = node.dirs.entry(part.to_vec()).or_default();
        }
    }

    fn write(&self, repo: &Repository) -> Result<ObjectId> {
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

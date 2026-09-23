//! `git stash`: save the dirty worktree + index as commits, then restore them.
//!
//! A stash entry is a commit exactly as Git builds it — no bespoke format:
//!
//! ```text
//!   i-commit  tree = staged index,   parent = [HEAD]
//!   w-commit  tree = full worktree,  parents = [HEAD, i-commit]
//! ```
//!
//! `refs/stash` points at the newest w-commit and its **reflog is the stack**:
//! `stash@{0}` is the newest reflog entry, `stash@{1}` the next, and so on —
//! which is why [`stash_list`] and [`stash_drop`] read and rewrite
//! `.git/logs/refs/stash` directly.
//!
//! Restoring (`apply`/`pop`) is a three-way merge of the stashed worktree tree
//! back onto the current HEAD (base = the HEAD the stash was taken against),
//! reusing the shared engine in [`crate::merge_cmd`].
//!
//! The CLI wiring lives in `main.rs`; the integrator calls [`cmd_stash_push`],
//! [`cmd_stash_pop`], [`cmd_stash_apply`], [`stash_list`], and [`cmd_stash_drop`].

use std::path::PathBuf;

use crate::error::{ForgeError, Result};
use crate::merge_cmd::{
    build_worktree_merge, force_reset, head_branch, install_unmerged, WorktreeMerge,
};
use crate::oid::ObjectId;
use crate::reflog::{log_path_for_ref, read_reflog, ReflogEntry};
use crate::repo::Repository;
use crate::signature::Signature;

const STASH_REF: &str = "refs/stash";
/// Upper bound on stash entries read from the reflog stack.
const MAX_STASH_ENTRIES: usize = 100_000;

/// Outcome of restoring a stash entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StashApply {
    /// The stash applied cleanly.
    Clean,
    /// Applying produced conflicts on these paths (index left unmerged).
    Conflicts(Vec<String>),
}

// ── push ────────────────────────────────────────────────────────────────────

/// `git stash push`: record the index and worktree as a stash entry, then reset
/// the worktree + index back to HEAD. Returns `None` when there is nothing to
/// stash (a clean worktree), or the new w-commit oid otherwise.
pub fn cmd_stash_push(
    repo: &Repository,
    sig: &Signature,
    message: Option<&str>,
) -> Result<Option<ObjectId>> {
    let head = repo
        .head()
        .map_err(|_| ForgeError::InvalidRef("cannot stash without an initial commit".into()))?;
    let head_commit = repo.peel_to_commit(head)?;
    let branch = head_branch(repo)
        .ok()
        .and_then(|refname| refname.strip_prefix("refs/heads/").map(String::from))
        .unwrap_or_else(|| "HEAD".to_string());

    // The staged tree (index as it stands) becomes the i-commit.
    let index_tree = repo.write_tree()?;

    // Stage the full worktree to capture unstaged changes; this rewrites the
    // index, but we reset it back to HEAD at the end regardless.
    repo.add(&[PathBuf::from(".")])?;
    let worktree_tree = repo.write_tree()?;

    // Nothing changed relative to HEAD → nothing to stash.
    if worktree_tree == head_commit.tree && index_tree == head_commit.tree {
        return Ok(None);
    }

    let short = &head.to_hex()[..7.min(head.to_hex().len())];
    let summary = head_commit.summary();
    let index_msg = format!("index on {branch}: {short} {summary}\n");
    let i_commit = repo.commit_tree(
        index_tree,
        vec![head],
        sig.clone(),
        sig.clone(),
        index_msg.as_bytes(),
        None,
    )?;

    let subject = match message {
        Some(text) => format!("On {branch}: {text}"),
        None => format!("WIP on {branch}: {short} {summary}"),
    };
    let w_commit = repo.commit_tree(
        worktree_tree,
        vec![head, i_commit],
        sig.clone(),
        sig.clone(),
        format!("{subject}\n").as_bytes(),
        None,
    )?;

    // Push onto the stack: advance refs/stash and append the reflog entry.
    let previous = repo.refs().resolve(STASH_REF).ok();
    repo.refs()
        .compare_and_swap(STASH_REF, previous, w_commit)?;
    repo.refs().append_reflog(
        STASH_REF, previous, w_commit, &sig.name, &sig.email, sig.time, &sig.tz, &subject,
    )?;

    // Restore the working tree + index to a clean HEAD.
    force_reset(repo, head_commit.tree)?;
    Ok(Some(w_commit))
}

// ── list ────────────────────────────────────────────────────────────────────

/// `git stash list`: the stack newest-first as `(index, "stash@{i}: <msg>")`.
pub fn stash_list(repo: &Repository) -> Result<Vec<(usize, String)>> {
    let entries = read_reflog(repo.git_dir(), STASH_REF, MAX_STASH_ENTRIES)?;
    Ok(entries
        .into_iter()
        .enumerate()
        .map(|(index, entry)| (index, format!("stash@{{{index}}}: {}", entry.message)))
        .collect())
}

// ── apply / pop ─────────────────────────────────────────────────────────────

/// `git stash apply [<index>]`: restore a stash entry without removing it.
pub fn cmd_stash_apply(repo: &Repository, index: usize) -> Result<StashApply> {
    let w_commit_oid = stash_commit(repo, index)?;
    apply_commit(repo, w_commit_oid)
}

/// `git stash pop [<index>]`: restore a stash entry and, if it applied cleanly,
/// drop it from the stack. A conflicting pop leaves the entry in place (like
/// Git) so the user does not lose it.
pub fn cmd_stash_pop(repo: &Repository, index: usize) -> Result<StashApply> {
    let outcome = cmd_stash_apply(repo, index)?;
    if matches!(outcome, StashApply::Clean) {
        cmd_stash_drop(repo, index)?;
    }
    Ok(outcome)
}

fn apply_commit(repo: &Repository, w_commit_oid: ObjectId) -> Result<StashApply> {
    let w_commit = repo.commit(&w_commit_oid)?;
    // The stash's first parent is the HEAD it was taken against — the base.
    let base_tree = match w_commit.parents.first() {
        Some(parent) => Some(repo.peel_to_commit(*parent)?.tree),
        None => None,
    };
    let ours_tree = repo.peel_to_commit(repo.head()?)?.tree;
    let theirs_tree = w_commit.tree;

    match build_worktree_merge(repo, base_tree, ours_tree, theirs_tree)? {
        WorktreeMerge::Clean(tree) => {
            repo.checkout_tree(tree)?;
            Ok(StashApply::Clean)
        }
        WorktreeMerge::Conflict {
            tree,
            conflicts,
            stages,
        } => {
            repo.checkout_tree(tree)?;
            install_unmerged(repo.git_dir(), &stages)?;
            Ok(StashApply::Conflicts(conflicts))
        }
    }
}

// ── drop ────────────────────────────────────────────────────────────────────

/// `git stash drop [<index>]`: remove one entry from the stack, rewriting the
/// `refs/stash` reflog and repointing (or deleting) the ref.
pub fn cmd_stash_drop(repo: &Repository, index: usize) -> Result<()> {
    let git_dir = repo.git_dir();
    let log_path = log_path_for_ref(git_dir, STASH_REF);
    let raw = std::fs::read_to_string(&log_path).map_err(|_| {
        ForgeError::InvalidRef(format!("stash@{{{index}}} not found (empty stash)"))
    })?;
    // Reflog file is oldest-first; the stack index is newest-first.
    let mut entries: Vec<ReflogEntry> = raw.lines().filter_map(ReflogEntry::parse).collect();
    let count = entries.len();
    if index >= count {
        return Err(ForgeError::InvalidRef(format!(
            "stash@{{{index}}} not found"
        )));
    }
    let file_pos = count - 1 - index;
    entries.remove(file_pos);

    if entries.is_empty() {
        // Stack emptied: delete the ref and its log.
        if let Ok(current) = repo.refs().resolve(STASH_REF) {
            let _ = repo.refs().compare_and_delete(STASH_REF, current);
        }
        let _ = std::fs::remove_file(&log_path);
        return Ok(());
    }

    // Repoint refs/stash at the new top (the last, newest reflog line).
    let new_top = ObjectId::from_hex(&entries.last().expect("non-empty").new_oid)?;
    let current = repo.refs().resolve(STASH_REF).ok();
    repo.refs().compare_and_swap(STASH_REF, current, new_top)?;

    let mut rewritten = String::new();
    for entry in &entries {
        rewritten.push_str(&entry.format_line());
        rewritten.push('\n');
    }
    std::fs::write(&log_path, rewritten)?;
    Ok(())
}

// ── helpers ─────────────────────────────────────────────────────────────────

/// The w-commit oid for `stash@{index}` (newest-first).
fn stash_commit(repo: &Repository, index: usize) -> Result<ObjectId> {
    let entries = read_reflog(repo.git_dir(), STASH_REF, MAX_STASH_ENTRIES)?;
    let entry = entries
        .get(index)
        .ok_or_else(|| ForgeError::InvalidRef(format!("stash@{{{index}}} not found")))?;
    ObjectId::from_hex(&entry.new_oid)
}

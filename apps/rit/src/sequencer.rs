//! The on-disk sequencer: rebase and cherry-pick as a resumable state machine.
//!
//! Git records multi-commit history edits under `.git/sequencer/` (a `todo`
//! list and a `done` log) plus the single-pick anchors `CHERRY_PICK_HEAD` /
//! `REBASE_HEAD`. This module reproduces that model on top of the from-scratch
//! three-way merge in [`crate::merge_cmd`]:
//!
//! - **cherry-pick** replays each named commit onto HEAD (base = the commit's
//!   first parent, ours = HEAD, theirs = the commit);
//! - **rebase** resets the branch to `onto` and replays `upstream..HEAD` on top;
//! - a conflict stops the sequence, leaving conflict markers + an unmerged index
//!   + `CHERRY_PICK_HEAD`, so the user can resolve and `--continue`, drop the
//!   commit with `--skip`, or unwind with `--abort`.
//!
//! State layout (all under the repository's git dir):
//!
//! ```text
//! sequencer/head    original HEAD oid (the abort target)
//! sequencer/onto    rebase target oid (rebase only)
//! sequencer/todo    remaining "pick <oid>" lines, oldest first
//! sequencer/done    applied "pick <oid>" lines
//! CHERRY_PICK_HEAD  the commit currently being applied (present on conflict)
//! MERGE_MSG         that commit's message (for --continue)
//! ```
//!
//! The CLI wiring lives in `main.rs`; the integrator calls [`cmd_cherry_pick`],
//! [`cmd_rebase`], [`cmd_sequencer_continue`], [`cmd_sequencer_abort`], and
//! [`cmd_sequencer_skip`].

use std::collections::HashSet;
use std::collections::VecDeque;

use crate::error::{ForgeError, Result};
use crate::merge_cmd::{
    build_worktree_merge, force_reset, head_branch, install_unmerged, read_gitdir_oid,
    read_gitdir_string, remove_gitdir_file, write_gitdir_file, WorktreeMerge,
};
use crate::object::Object;
use crate::oid::ObjectId;
use crate::repo::Repository;
use crate::signature::Signature;

const TODO: &str = "sequencer/todo";
const DONE: &str = "sequencer/done";
const HEAD_FILE: &str = "sequencer/head";
const ONTO: &str = "sequencer/onto";
const CHERRY_PICK_HEAD: &str = "CHERRY_PICK_HEAD";
const REBASE_HEAD: &str = "REBASE_HEAD";
const MERGE_MSG: &str = "MERGE_MSG";

/// Bound on the ancestry walk used to compute the rebase commit list.
const MAX_REBASE_WALK: usize = 500_000;

/// Whether a sequence ran to completion or stopped on a conflict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SequencerStatus {
    /// Every step applied; no state remains.
    Completed,
    /// Stopped on a conflict on these paths; state is persisted for `--continue`.
    Stopped { conflicts: Vec<String> },
}

// ── cherry-pick ─────────────────────────────────────────────────────────────

/// `git cherry-pick <commit>...`: replay each commit onto the current branch.
pub fn cmd_cherry_pick(
    repo: &Repository,
    revs: &[&str],
    sig: &Signature,
) -> Result<SequencerStatus> {
    if revs.is_empty() {
        return Err(ForgeError::InvalidRef(
            "cherry-pick requires at least one commit".into(),
        ));
    }
    let orig = repo.head().map_err(|_| {
        ForgeError::InvalidRef("cannot cherry-pick onto an empty repository".into())
    })?;
    head_branch(repo)?; // fail fast on detached HEAD

    let mut oids = Vec::with_capacity(revs.len());
    for rev in revs {
        oids.push(peel_commit(repo, repo.resolve(rev)?)?);
    }

    let git_dir = repo.git_dir();
    write_gitdir_file(git_dir, HEAD_FILE, format!("{orig}\n").as_bytes())?;
    remove_gitdir_file(git_dir, ONTO);
    write_todo(repo, &oids)?;
    write_gitdir_file(git_dir, DONE, b"")?;

    run_todo(repo, sig)
}

// ── rebase ──────────────────────────────────────────────────────────────────

/// `git rebase [--onto <onto>] <upstream>`: reset the current branch to `onto`
/// (default `upstream`) and replay `upstream..HEAD` on top.
pub fn cmd_rebase(
    repo: &Repository,
    upstream: &str,
    onto: Option<&str>,
    sig: &Signature,
) -> Result<SequencerStatus> {
    let head_ref = head_branch(repo)?;
    let head_oid = repo
        .head()
        .map_err(|_| ForgeError::InvalidRef("cannot rebase an empty repository".into()))?;
    let upstream_oid = peel_commit(repo, repo.resolve(upstream)?)?;
    let onto_oid = match onto {
        Some(spec) => peel_commit(repo, repo.resolve(spec)?)?,
        None => upstream_oid,
    };

    let commits = commits_to_replay(repo, upstream_oid, head_oid)?;

    let git_dir = repo.git_dir();
    write_gitdir_file(git_dir, HEAD_FILE, format!("{head_oid}\n").as_bytes())?;
    write_gitdir_file(git_dir, ONTO, format!("{onto_oid}\n").as_bytes())?;
    write_todo(repo, &commits)?;
    write_gitdir_file(git_dir, DONE, b"")?;

    // Detach the worktree/branch onto the new base before replaying.
    if onto_oid != head_oid {
        repo.checkout_tree(repo.peel_to_commit(onto_oid)?.tree)?;
        repo.refs()
            .compare_and_swap(&head_ref, Some(head_oid), onto_oid)?;
        let _ = repo.refs().append_reflog(
            &head_ref,
            Some(head_oid),
            onto_oid,
            &sig.name,
            &sig.email,
            sig.time,
            &sig.tz,
            "rebase: checkout onto",
        );
    }

    run_todo(repo, sig)
}

// ── continue / abort / skip ─────────────────────────────────────────────────

/// `--continue`: finalize the in-progress pick (from the now-resolved index),
/// then resume the remaining todo. Also handles finishing a conflicted merge.
pub fn cmd_sequencer_continue(repo: &Repository, sig: &Signature) -> Result<SequencerStatus> {
    let git_dir = repo.git_dir();

    if read_gitdir_oid(git_dir, CHERRY_PICK_HEAD).is_some() {
        finalize_pick(repo, sig)?;
    } else if read_gitdir_oid(git_dir, "MERGE_HEAD").is_some() {
        // A plain `merge` conflict was resolved; complete it.
        crate::merge_cmd::continue_merge(repo, sig, None)?;
        return Ok(SequencerStatus::Completed);
    }

    if git_dir.join(TODO).exists() {
        run_todo(repo, sig)
    } else {
        finish(repo);
        Ok(SequencerStatus::Completed)
    }
}

/// `--abort`: unwind to the original HEAD recorded at the start of the sequence.
pub fn cmd_sequencer_abort(repo: &Repository) -> Result<()> {
    let git_dir = repo.git_dir();
    let orig = read_gitdir_oid(git_dir, HEAD_FILE)
        .or_else(|| read_gitdir_oid(git_dir, "ORIG_HEAD"))
        .ok_or_else(|| ForgeError::InvalidRef("no sequencer state to abort".into()))?;
    let head_ref = head_branch(repo)?;
    let current = repo.head()?;
    if current != orig {
        repo.refs()
            .compare_and_swap(&head_ref, Some(current), orig)?;
    }
    force_reset(repo, repo.peel_to_commit(orig)?.tree)?;
    finish(repo);
    Ok(())
}

/// `--skip`: drop the conflicted commit and resume the remaining todo.
pub fn cmd_sequencer_skip(repo: &Repository, sig: &Signature) -> Result<SequencerStatus> {
    let git_dir = repo.git_dir();
    // Discard the half-applied worktree/index back to the current HEAD.
    let head_tree = repo.peel_to_commit(repo.head()?)?.tree;
    force_reset(repo, head_tree)?;
    remove_gitdir_file(git_dir, CHERRY_PICK_HEAD);
    remove_gitdir_file(git_dir, REBASE_HEAD);
    remove_gitdir_file(git_dir, MERGE_MSG);

    if git_dir.join(TODO).exists() {
        run_todo(repo, sig)
    } else {
        finish(repo);
        Ok(SequencerStatus::Completed)
    }
}

// ── core loop ───────────────────────────────────────────────────────────────

/// Pop and apply todo entries until the list drains or a pick conflicts.
fn run_todo(repo: &Repository, sig: &Signature) -> Result<SequencerStatus> {
    loop {
        let todo = read_todo(repo)?;
        let Some((&next, rest)) = todo.split_first() else {
            finish(repo);
            return Ok(SequencerStatus::Completed);
        };
        // Persist the remaining list *before* attempting, so a conflict leaves a
        // correct resume point (the current pick lives in CHERRY_PICK_HEAD).
        write_todo(repo, rest)?;

        let head = repo.head()?;
        match pick_onto(repo, head, next, sig)? {
            PickOutcome::Applied | PickOutcome::Empty => {
                append_done(repo, next)?;
            }
            PickOutcome::Conflicted(conflicts) => {
                write_gitdir_file(repo.git_dir(), REBASE_HEAD, format!("{next}\n").as_bytes())?;
                return Ok(SequencerStatus::Stopped { conflicts });
            }
        }
    }
}

enum PickOutcome {
    Applied,
    /// The pick produced no change (already applied) — silently skipped.
    Empty,
    Conflicted(Vec<String>),
}

/// Apply one commit on top of `head` using three-way merge semantics.
///
/// On a clean apply, a new commit is written (author preserved from the picked
/// commit, committer = `sig`) and the branch advances. On a conflict, the
/// worktree/index are left conflicted and `CHERRY_PICK_HEAD` is recorded.
fn pick_onto(
    repo: &Repository,
    head: ObjectId,
    commit_oid: ObjectId,
    sig: &Signature,
) -> Result<PickOutcome> {
    let commit = repo.commit(&commit_oid)?;
    let base_tree = match commit.parents.first() {
        Some(parent) => Some(repo.peel_to_commit(*parent)?.tree),
        None => None,
    };
    let ours_tree = repo.peel_to_commit(head)?.tree;
    let theirs_tree = commit.tree;
    let head_ref = head_branch(repo)?;

    match build_worktree_merge(repo, base_tree, ours_tree, theirs_tree)? {
        WorktreeMerge::Clean(tree) => {
            if tree == ours_tree {
                return Ok(PickOutcome::Empty);
            }
            repo.checkout_tree(tree)?;
            let message = if commit.message.is_empty() {
                b"cherry-pick\n".to_vec()
            } else {
                commit.message.clone()
            };
            // The commit is written and the branch ref advanced as a side effect;
            // the sequencer resumes from the ref, so the new oid isn't needed here.
            repo.commit_tree(
                tree,
                vec![head],
                commit.author.clone(),
                sig.clone(),
                &message,
                Some(&head_ref),
            )?;
            Ok(PickOutcome::Applied)
        }
        WorktreeMerge::Conflict {
            tree,
            conflicts,
            stages,
        } => {
            repo.checkout_tree(tree)?;
            install_unmerged(repo.git_dir(), &stages)?;
            write_gitdir_file(
                repo.git_dir(),
                CHERRY_PICK_HEAD,
                format!("{commit_oid}\n").as_bytes(),
            )?;
            let message = if commit.message.is_empty() {
                b"cherry-pick\n".to_vec()
            } else {
                commit.message.clone()
            };
            write_gitdir_file(repo.git_dir(), MERGE_MSG, &message)?;
            Ok(PickOutcome::Conflicted(conflicts))
        }
    }
}

/// Create the commit for a conflict that the user has now resolved and staged.
fn finalize_pick(repo: &Repository, sig: &Signature) -> Result<ObjectId> {
    let git_dir = repo.git_dir();
    let cherry = read_gitdir_oid(git_dir, CHERRY_PICK_HEAD)
        .ok_or_else(|| ForgeError::InvalidRef("no CHERRY_PICK_HEAD to continue".into()))?;
    let picked = repo.commit(&cherry)?;
    let head = repo.head()?;
    let head_ref = head_branch(repo)?;
    // Fails closed unless every conflicted path has been resolved to stage 0.
    let tree = repo.write_tree()?;
    let message = read_gitdir_string(git_dir, MERGE_MSG)
        .map(String::into_bytes)
        .unwrap_or_else(|| picked.message.clone());
    let message = if message.is_empty() {
        b"cherry-pick\n".to_vec()
    } else {
        message
    };
    let new = repo.commit_tree(
        tree,
        vec![head],
        picked.author.clone(),
        sig.clone(),
        &message,
        Some(&head_ref),
    )?;
    append_done(repo, cherry)?;
    remove_gitdir_file(git_dir, CHERRY_PICK_HEAD);
    remove_gitdir_file(git_dir, REBASE_HEAD);
    remove_gitdir_file(git_dir, MERGE_MSG);
    Ok(new)
}

// ── todo/done persistence ───────────────────────────────────────────────────

fn write_todo(repo: &Repository, oids: &[ObjectId]) -> Result<()> {
    write_gitdir_file(repo.git_dir(), TODO, &encode_list(oids))
}

fn read_todo(repo: &Repository) -> Result<Vec<ObjectId>> {
    match read_gitdir_string(repo.git_dir(), TODO) {
        Some(content) => decode_list(&content),
        None => Ok(Vec::new()),
    }
}

fn append_done(repo: &Repository, oid: ObjectId) -> Result<()> {
    let mut done = read_gitdir_string(repo.git_dir(), DONE).unwrap_or_default();
    done.push_str(&format!("pick {oid}\n"));
    write_gitdir_file(repo.git_dir(), DONE, done.as_bytes())
}

fn encode_list(oids: &[ObjectId]) -> Vec<u8> {
    let mut out = String::new();
    for oid in oids {
        out.push_str(&format!("pick {oid}\n"));
    }
    out.into_bytes()
}

fn decode_list(content: &str) -> Result<Vec<ObjectId>> {
    let mut oids = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // "pick <oid>" (accept a bare oid too).
        let token = line.split_whitespace().nth(1).unwrap_or(line);
        oids.push(ObjectId::from_hex(token)?);
    }
    Ok(oids)
}

/// Remove every sequencer state file at the end of (or on abort of) a sequence.
fn finish(repo: &Repository) {
    let git_dir = repo.git_dir();
    remove_gitdir_file(git_dir, TODO);
    remove_gitdir_file(git_dir, DONE);
    remove_gitdir_file(git_dir, HEAD_FILE);
    remove_gitdir_file(git_dir, ONTO);
    remove_gitdir_file(git_dir, CHERRY_PICK_HEAD);
    remove_gitdir_file(git_dir, REBASE_HEAD);
    remove_gitdir_file(git_dir, MERGE_MSG);
    let _ = std::fs::remove_dir(git_dir.join("sequencer"));
}

// ── revision helpers ────────────────────────────────────────────────────────

fn peel_commit(repo: &Repository, oid: ObjectId) -> Result<ObjectId> {
    crate::merge_cmd::peel_commit_oid(repo, oid)
}

/// The commits reachable from `head` along first-parent history but not already
/// contained in `upstream`, ordered oldest → newest (the rebase replay order).
fn commits_to_replay(
    repo: &Repository,
    upstream: ObjectId,
    head: ObjectId,
) -> Result<Vec<ObjectId>> {
    let upstream_ancestors = ancestors(repo, upstream)?;
    let mut list = Vec::new();
    let mut current = head;
    loop {
        if upstream_ancestors.contains(&current) {
            break;
        }
        let commit = match repo.read_object(&current)? {
            Object::Commit(commit) => commit,
            _ => break,
        };
        list.push(current);
        if list.len() > MAX_REBASE_WALK {
            return Err(ForgeError::ResourceLimit(
                "rebase commit list exceeds limit".into(),
            ));
        }
        match commit.parents.first() {
            Some(parent) => current = *parent,
            None => break,
        }
    }
    list.reverse();
    Ok(list)
}

/// All ancestors of `start` (inclusive), bounded.
fn ancestors(repo: &Repository, start: ObjectId) -> Result<HashSet<ObjectId>> {
    let mut set = HashSet::new();
    let mut queue = VecDeque::from([start]);
    while let Some(oid) = queue.pop_front() {
        if !set.insert(oid) {
            continue;
        }
        if set.len() > MAX_REBASE_WALK {
            return Err(ForgeError::ResourceLimit(
                "rebase ancestry walk exceeds limit".into(),
            ));
        }
        if let Ok(Object::Commit(commit)) = repo.read_object(&oid) {
            for parent in commit.parents {
                queue.push_back(parent);
            }
        }
    }
    Ok(set)
}

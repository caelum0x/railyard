//! The git-backed half of pull requests: commit lists, three-dot diffs,
//! mergeability analysis, and the merge itself.
//!
//! - **Diff** uses `rit::diff_trees` (a from-scratch tree diff) to find changed
//!   paths, then renders each text blob to a unified patch with `similar`.
//! - **Mergeability** uses `rit::is_ancestor` for up-to-date/fast-forward
//!   detection and, for a true three-way merge, `rit::merge_tree` (which mounts
//!   `git merge-tree` — clearly a Git-backed bridge, since `rit` has no native
//!   three-way merge yet).
//! - **Merge** either fast-forwards the base ref (atomic CAS) or writes a real
//!   two-parent merge commit via `rit::Repository::commit_tree` onto the merged
//!   tree, then advances the base ref.

use std::collections::{HashSet, VecDeque};

use rit::object::Object;
use rit::{ConnectivityLimits, ObjectId, Repository, Signature};
use similar::{ChangeTag, TextDiff};

use crate::error::{GitHostError, Result};
use crate::RepoHost;

/// A single file's change within a diff.
pub struct FileDiff {
    pub path: String,
    /// `A`dded, `M`odified, or `D`eleted.
    pub status: char,
    pub old_oid: Option<String>,
    pub new_oid: Option<String>,
    pub additions: usize,
    pub deletions: usize,
    pub binary: bool,
    /// A unified-diff patch (empty for binary changes).
    pub patch: String,
}

/// A computed diff between two trees.
pub struct DiffResult {
    pub files: Vec<FileDiff>,
    pub additions: usize,
    pub deletions: usize,
}

/// One commit in a PR's commit list.
pub struct CommitInfo {
    pub oid: String,
    pub summary: String,
    pub author_name: String,
    pub author_email: String,
    pub message: String,
    /// Author timestamp (unix seconds).
    pub time: i64,
}

/// Mergeability verdict for a base←head pair.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeStatus {
    /// Head is already contained in base — nothing to merge.
    UpToDate,
    /// Base is an ancestor of head — a fast-forward is possible.
    FastForward,
    /// Histories diverged but merge cleanly into a merge commit.
    MergeCommit,
    /// Histories diverged and the three-way merge conflicts.
    Conflict,
}

impl MergeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            MergeStatus::UpToDate => "up_to_date",
            MergeStatus::FastForward => "fast_forward",
            MergeStatus::MergeCommit => "merge_commit",
            MergeStatus::Conflict => "conflict",
        }
    }
    /// Whether a merge can proceed.
    pub fn mergeable(&self) -> bool {
        matches!(self, MergeStatus::FastForward | MergeStatus::MergeCommit)
    }
}

/// The result of a mergeability analysis.
pub struct MergeAnalysis {
    pub status: MergeStatus,
    pub base_sha: String,
    pub head_sha: String,
    pub merge_base: Option<String>,
    pub conflict_info: Option<String>,
}

/// The result of performing a merge.
#[derive(Debug)]
pub struct MergeResult {
    /// The merge commit (real merge) or head tip (fast-forward).
    pub merge_commit_sha: String,
    /// The base ref's new tip.
    pub new_base_sha: String,
    /// `"fast-forward"`, `"merge"`, `"squash"`, or `"rebase"`.
    pub kind: &'static str,
}

/// The strategy used to integrate a pull request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MergeStrategy {
    /// Fast-forward when possible, else a two-parent merge commit.
    Merge,
    /// A single commit of the PR's net changes on top of base.
    Squash,
    /// Replay the PR's commits onto base (linear history, new SHAs).
    Rebase,
}

impl MergeStrategy {
    /// Parse a strategy name, defaulting to `Merge`.
    pub fn parse(value: &str) -> Self {
        match value {
            "squash" => MergeStrategy::Squash,
            "rebase" => MergeStrategy::Rebase,
            _ => MergeStrategy::Merge,
        }
    }
}

const MAX_HISTORY: usize = 500_000;

impl RepoHost {
    /// Resolve a branch to its tip commit id.
    pub fn resolve_branch(&self, org: &str, repo: &str, branch: &str) -> Result<ObjectId> {
        let repository = self.open(org, repo)?;
        let refname = format!("refs/heads/{branch}");
        repository
            .refs()
            .resolve(&refname)
            .map_err(|_| GitHostError::NotFound(format!("branch {branch}")))
    }

    /// The commits reachable from `head` but not from `base` (the PR's commits),
    /// newest first.
    pub fn commit_list(
        &self,
        org: &str,
        repo: &str,
        base: ObjectId,
        head: ObjectId,
    ) -> Result<Vec<CommitInfo>> {
        let repository = self.open(org, repo)?;
        let base_ancestors = ancestors(&repository, base)?;
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from([head]);
        while let Some(oid) = queue.pop_front() {
            if base_ancestors.contains(&oid) || !seen.insert(oid) {
                continue;
            }
            let commit = repository.commit(&oid)?;
            out.push(CommitInfo {
                oid: oid.to_hex(),
                summary: commit.summary(),
                author_name: commit.author.name.clone(),
                author_email: commit.author.email.clone(),
                message: commit.message_str().into_owned(),
                time: commit.author.time,
            });
            for parent in &commit.parents {
                queue.push_back(*parent);
            }
            if out.len() > MAX_HISTORY {
                break;
            }
        }
        // Newest first by committer time.
        Ok(out)
    }

    /// The three-dot diff for a PR: changes on `head` relative to the merge base
    /// with `base` (so unrelated base-side commits are not shown as reversions).
    pub fn pr_diff(&self, org: &str, repo: &str, base: ObjectId, head: ObjectId) -> Result<DiffResult> {
        let repository = self.open(org, repo)?;
        let merge_base = merge_base(&repository, base, head)?.unwrap_or(base);
        let old_tree = tree_of(&repository, merge_base)?;
        let new_tree = tree_of(&repository, head)?;
        self.render_diff(&repository, Some(old_tree), Some(new_tree))
    }

    /// Render a tree-to-tree diff into per-file unified patches.
    pub(crate) fn render_diff(
        &self,
        repository: &Repository,
        old_tree: Option<ObjectId>,
        new_tree: Option<ObjectId>,
    ) -> Result<DiffResult> {
        let changes = rit::diff_trees(repository.odb(), old_tree, new_tree)?;
        let mut files = Vec::with_capacity(changes.len());
        let mut total_add = 0;
        let mut total_del = 0;
        for change in changes {
            let path = String::from_utf8_lossy(&change.path).into_owned();
            let old_bytes = match change.old_oid {
                Some(oid) => repository.blob(&oid).ok().map(|b| b.data),
                None => None,
            };
            let new_bytes = match change.new_oid {
                Some(oid) => repository.blob(&oid).ok().map(|b| b.data),
                None => None,
            };
            let binary = is_binary(old_bytes.as_deref()) || is_binary(new_bytes.as_deref());
            let (patch, additions, deletions) = if binary {
                (String::new(), 0, 0)
            } else {
                unified_patch(
                    &path,
                    old_bytes.as_deref().unwrap_or(b""),
                    new_bytes.as_deref().unwrap_or(b""),
                )
            };
            total_add += additions;
            total_del += deletions;
            files.push(FileDiff {
                path,
                status: change.kind.code(),
                old_oid: change.old_oid.map(|o| o.to_hex()),
                new_oid: change.new_oid.map(|o| o.to_hex()),
                additions,
                deletions,
                binary,
                patch,
            });
        }
        Ok(DiffResult {
            files,
            additions: total_add,
            deletions: total_del,
        })
    }

    /// Analyze whether `head` can merge into `base`.
    pub fn mergeability(
        &self,
        org: &str,
        repo: &str,
        base: ObjectId,
        head: ObjectId,
    ) -> Result<MergeAnalysis> {
        let repository = self.open(org, repo)?;
        let limits = ConnectivityLimits::default();
        let base_hex = base.to_hex();
        let head_hex = head.to_hex();
        let mb = merge_base(&repository, base, head)?.map(|o| o.to_hex());

        // Head already in base (equal or head is an ancestor of base).
        if base == head || rit::is_ancestor(repository.odb(), head, base, limits)? {
            return Ok(MergeAnalysis {
                status: MergeStatus::UpToDate,
                base_sha: base_hex,
                head_sha: head_hex,
                merge_base: mb,
                conflict_info: None,
            });
        }
        // Base is an ancestor of head → fast-forward.
        if rit::is_ancestor(repository.odb(), base, head, limits)? {
            return Ok(MergeAnalysis {
                status: MergeStatus::FastForward,
                base_sha: base_hex,
                head_sha: head_hex,
                merge_base: mb,
                conflict_info: None,
            });
        }
        // Diverged: run rit's native three-way merge to classify clean vs conflict.
        match rit::merge_commits(&repository, base, head)? {
            rit::MergeOutcome::Clean(_) => Ok(MergeAnalysis {
                status: MergeStatus::MergeCommit,
                base_sha: base_hex,
                head_sha: head_hex,
                merge_base: mb,
                conflict_info: None,
            }),
            rit::MergeOutcome::Conflict(paths) => Ok(MergeAnalysis {
                status: MergeStatus::Conflict,
                base_sha: base_hex,
                head_sha: head_hex,
                merge_base: mb,
                conflict_info: Some(format!("conflicting paths: {}", paths.join(", "))),
            }),
        }
    }

    /// Integrate `head` (a commit oid — a same-repo branch tip or a cross-repo
    /// pull-request head already imported into this repo) into `base_branch`
    /// using `strategy`. Advances the base ref atomically and refreshes
    /// server-info. All strategies are computed natively (no `git` binary).
    #[allow(clippy::too_many_arguments)]
    pub fn merge(
        &self,
        org: &str,
        repo: &str,
        base_branch: &str,
        head: ObjectId,
        strategy: MergeStrategy,
        author_name: &str,
        author_email: &str,
        message: &str,
    ) -> Result<MergeResult> {
        let repository = self.open(org, repo)?;
        let base = self.resolve_branch(org, repo, base_branch)?;
        let base_ref = format!("refs/heads/{base_branch}");
        let analysis = self.mergeability(org, repo, base, head)?;

        if analysis.status == MergeStatus::UpToDate {
            return Err(GitHostError::Invalid("nothing to merge".into()));
        }
        if analysis.status == MergeStatus::Conflict {
            return Err(GitHostError::Invalid("merge conflicts".into()));
        }

        let signature = signature(author_name, author_email)?;
        let result = match strategy {
            MergeStrategy::Merge => {
                if analysis.status == MergeStatus::FastForward {
                    repository
                        .refs()
                        .compare_and_swap(&base_ref, Some(base), head)
                        .map_err(GitHostError::Rit)?;
                    MergeResult {
                        merge_commit_sha: head.to_hex(),
                        new_base_sha: head.to_hex(),
                        kind: "fast-forward",
                    }
                } else {
                    let tree = self.merged_tree(&repository, base, head)?;
                    let merge_commit = repository
                        .commit_tree(
                            tree,
                            vec![base, head],
                            signature.clone(),
                            signature,
                            message.as_bytes(),
                            Some(&base_ref),
                        )
                        .map_err(GitHostError::Rit)?;
                    MergeResult {
                        merge_commit_sha: merge_commit.to_hex(),
                        new_base_sha: merge_commit.to_hex(),
                        kind: "merge",
                    }
                }
            }
            MergeStrategy::Squash => {
                // One commit of the PR's net changes, single-parent on base.
                let tree = self.merged_tree(&repository, base, head)?;
                let squashed = repository
                    .commit_tree(
                        tree,
                        vec![base],
                        signature.clone(),
                        signature,
                        message.as_bytes(),
                        Some(&base_ref),
                    )
                    .map_err(GitHostError::Rit)?;
                MergeResult {
                    merge_commit_sha: squashed.to_hex(),
                    new_base_sha: squashed.to_hex(),
                    kind: "squash",
                }
            }
            MergeStrategy::Rebase => {
                let new_tip = self.rebase_onto(&repository, base, head)?;
                repository
                    .refs()
                    .compare_and_swap(&base_ref, Some(base), new_tip)
                    .map_err(GitHostError::Rit)?;
                MergeResult {
                    merge_commit_sha: new_tip.to_hex(),
                    new_base_sha: new_tip.to_hex(),
                    kind: "rebase",
                }
            }
        };

        let _ = rit::update_server_info(&repository);
        Ok(result)
    }

    /// The tree resulting from three-way merging `head` into `base`.
    fn merged_tree(&self, repository: &Repository, base: ObjectId, head: ObjectId) -> Result<ObjectId> {
        match rit::merge_commits(repository, base, head)? {
            rit::MergeOutcome::Clean(tree) => Ok(tree),
            rit::MergeOutcome::Conflict(_) => Err(GitHostError::Invalid("merge conflicts".into())),
        }
    }

    /// Cherry-pick each of `head`'s commits (not in `base`) onto `base`,
    /// producing a new linear tip. Each pick is a three-way tree merge of
    /// (commit's parent, current new tip, commit); a conflict aborts the rebase.
    fn rebase_onto(&self, repository: &Repository, base: ObjectId, head: ObjectId) -> Result<ObjectId> {
        let mut picks = self.commits_between(repository, base, head)?;
        picks.reverse(); // oldest first
        let mut new_parent = base;
        for oid in picks {
            let commit = repository.commit(&oid)?;
            let parent_tree = match commit.parents.first() {
                Some(parent) => Some(repository.commit(parent)?.tree),
                None => None,
            };
            let ours_tree = repository.commit(&new_parent)?.tree;
            let merged = match rit::merge_trees(repository, parent_tree, ours_tree, commit.tree)? {
                rit::MergeOutcome::Clean(tree) => tree,
                rit::MergeOutcome::Conflict(_) => {
                    return Err(GitHostError::Invalid("rebase conflict".into()))
                }
            };
            new_parent = repository
                .commit_tree(
                    merged,
                    vec![new_parent],
                    commit.author.clone(),
                    commit.committer.clone(),
                    &commit.message,
                    None,
                )
                .map_err(GitHostError::Rit)?;
        }
        Ok(new_parent)
    }

    /// Commit oids reachable from `head` but not `base`, newest first.
    fn commits_between(
        &self,
        repository: &Repository,
        base: ObjectId,
        head: ObjectId,
    ) -> Result<Vec<ObjectId>> {
        let base_ancestors = ancestors(repository, base)?;
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let mut queue = VecDeque::from([head]);
        while let Some(oid) = queue.pop_front() {
            if base_ancestors.contains(&oid) || !seen.insert(oid) {
                continue;
            }
            let commit = repository.commit(&oid)?;
            out.push(oid);
            for parent in &commit.parents {
                queue.push_back(*parent);
            }
        }
        Ok(out)
    }
}

/// The tree id of a commit.
fn tree_of(repository: &Repository, commit: ObjectId) -> Result<ObjectId> {
    Ok(repository.peel_to_commit(commit)?.tree)
}

/// All ancestors of `start` (inclusive), bounded.
fn ancestors(repository: &Repository, start: ObjectId) -> Result<HashSet<ObjectId>> {
    let mut set = HashSet::new();
    let mut queue = VecDeque::from([start]);
    while let Some(oid) = queue.pop_front() {
        if !set.insert(oid) {
            continue;
        }
        if set.len() > MAX_HISTORY {
            break;
        }
        if let Ok(Object::Commit(commit)) = repository.odb().read(&oid) {
            for parent in commit.parents {
                queue.push_back(parent);
            }
        }
    }
    Ok(set)
}

/// The closest common ancestor of `a` and `b` (a merge base), if any.
fn merge_base(repository: &Repository, a: ObjectId, b: ObjectId) -> Result<Option<ObjectId>> {
    let a_ancestors = ancestors(repository, a)?;
    let mut seen = HashSet::new();
    let mut queue = VecDeque::from([b]);
    while let Some(oid) = queue.pop_front() {
        if !seen.insert(oid) {
            continue;
        }
        if a_ancestors.contains(&oid) {
            return Ok(Some(oid));
        }
        if let Ok(Object::Commit(commit)) = repository.odb().read(&oid) {
            for parent in commit.parents {
                queue.push_back(parent);
            }
        }
    }
    Ok(None)
}

/// Build a signature from a name/email at the current time.
fn signature(name: &str, email: &str) -> Result<Signature> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Signature::parse(&format!("{name} <{email}> {seconds} +0000"), "commit")
        .map_err(GitHostError::Rit)
}

/// A blob is treated as binary if it contains a NUL byte.
fn is_binary(data: Option<&[u8]>) -> bool {
    data.map(|d| d.contains(&0)).unwrap_or(false)
}

/// Produce a unified-diff patch and (additions, deletions) counts.
fn unified_patch(path: &str, old: &[u8], new: &[u8]) -> (String, usize, usize) {
    let old_str = String::from_utf8_lossy(old);
    let new_str = String::from_utf8_lossy(new);
    let diff = TextDiff::from_lines(old_str.as_ref(), new_str.as_ref());
    let mut additions = 0;
    let mut deletions = 0;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Insert => additions += 1,
            ChangeTag::Delete => deletions += 1,
            ChangeTag::Equal => {}
        }
    }
    let patch = diff
        .unified_diff()
        .context_radius(3)
        .header(&format!("a/{path}"), &format!("b/{path}"))
        .to_string();
    (patch, additions, deletions)
}

#[cfg(test)]
mod tests {
    //! Merge-engine tests over real, on-disk bare repos (tempdir; no DB). These
    //! exercise the trust-critical path a protected-branch gate defends: what a
    //! `merge` actually integrates for each strategy and mergeability verdict.
    use super::{MergeStatus, MergeStrategy};
    use crate::{RepoHost, SeedFile};
    use rit::ObjectId;

    const ORG: &str = "acme";
    const REPO: &str = "widgets";

    fn new_host() -> (tempfile::TempDir, RepoHost) {
        let dir = tempfile::tempdir().unwrap();
        let host = RepoHost::new(dir.path());
        host.init_bare(ORG, REPO, "main").unwrap();
        (dir, host)
    }

    /// Commit `files` onto `branch` (parent = the branch's current tip, if any).
    fn seed(host: &RepoHost, branch: &str, files: &[(&str, &str)], msg: &str) -> ObjectId {
        let seed_files: Vec<SeedFile> = files
            .iter()
            .map(|(name, content)| SeedFile {
                name: (*name).into(),
                content: content.as_bytes().to_vec(),
                executable: false,
            })
            .collect();
        let hex = host
            .seed_commit(ORG, REPO, branch, &seed_files, msg, "Ada", "ada@example.com")
            .unwrap();
        ObjectId::from_hex(&hex).unwrap()
    }

    /// Point `branch` at `oid` (used to fork a branch off a known commit).
    fn set_branch(host: &RepoHost, branch: &str, oid: ObjectId) {
        let repository = host.open(ORG, REPO).unwrap();
        let refname = format!("refs/heads/{branch}");
        let expected = repository.refs().resolve(&refname).ok();
        repository
            .refs()
            .compare_and_swap(&refname, expected, oid)
            .unwrap();
    }

    fn parents_of(host: &RepoHost, oid: ObjectId) -> Vec<ObjectId> {
        host.open(ORG, REPO).unwrap().commit(&oid).unwrap().parents
    }

    fn tree_of(host: &RepoHost, oid: ObjectId) -> ObjectId {
        host.open(ORG, REPO).unwrap().commit(&oid).unwrap().tree
    }

    /// main=c1; feature is strictly ahead → fast-forward advances base to head.
    #[test]
    fn fast_forward_merge_advances_base_to_head() {
        let (_dir, host) = new_host();
        let c1 = seed(&host, "main", &[("base.txt", "base\n")], "c1");
        set_branch(&host, "feature", c1);
        let feat = seed(&host, "feature", &[("base.txt", "base\n"), ("x.txt", "x\n")], "c2");

        let analysis = host.mergeability(ORG, REPO, c1, feat).unwrap();
        assert_eq!(analysis.status, MergeStatus::FastForward);

        let result = host
            .merge(ORG, REPO, "main", feat, MergeStrategy::Merge, "Ada", "ada@x", "ff")
            .unwrap();
        assert_eq!(result.kind, "fast-forward");
        assert_eq!(result.new_base_sha, feat.to_hex());
        assert_eq!(host.resolve_branch(ORG, REPO, "main").unwrap(), feat);
    }

    /// Divergent, disjoint changes → a two-parent merge commit on base.
    #[test]
    fn divergent_history_produces_two_parent_merge() {
        let (_dir, host) = new_host();
        let c1 = seed(&host, "main", &[("base.txt", "base\n")], "c1");
        set_branch(&host, "feature", c1);
        let feat = seed(&host, "feature", &[("base.txt", "base\n"), ("f.txt", "f\n")], "feat");
        let main = seed(&host, "main", &[("base.txt", "base\n"), ("m.txt", "m\n")], "main2");

        let analysis = host.mergeability(ORG, REPO, main, feat).unwrap();
        assert_eq!(analysis.status, MergeStatus::MergeCommit);

        let result = host
            .merge(ORG, REPO, "main", feat, MergeStrategy::Merge, "Ada", "ada@x", "merge")
            .unwrap();
        assert_eq!(result.kind, "merge");
        let merge_oid = ObjectId::from_hex(&result.merge_commit_sha).unwrap();
        // A real merge commit has exactly the two integrated tips as parents.
        let parents = parents_of(&host, merge_oid);
        assert_eq!(parents.len(), 2);
        assert!(parents.contains(&main) && parents.contains(&feat));
        assert_eq!(host.resolve_branch(ORG, REPO, "main").unwrap(), merge_oid);
    }

    /// Both sides edit the same file differently → conflict, and merge refuses.
    #[test]
    fn conflicting_change_is_rejected() {
        let (_dir, host) = new_host();
        let c1 = seed(&host, "main", &[("base.txt", "base\n")], "c1");
        set_branch(&host, "feature", c1);
        let feat = seed(&host, "feature", &[("base.txt", "feature edit\n")], "feat");
        let main = seed(&host, "main", &[("base.txt", "main edit\n")], "main2");

        let analysis = host.mergeability(ORG, REPO, main, feat).unwrap();
        assert_eq!(analysis.status, MergeStatus::Conflict);

        let err = host
            .merge(ORG, REPO, "main", feat, MergeStrategy::Merge, "Ada", "ada@x", "merge")
            .unwrap_err();
        assert!(matches!(err, crate::GitHostError::Invalid(_)));
        // Base must not have moved.
        assert_eq!(host.resolve_branch(ORG, REPO, "main").unwrap(), main);
    }

    /// Squash integrates the net tree as a single-parent commit on base.
    #[test]
    fn squash_creates_single_parent_commit_with_merged_tree() {
        let (_dir, host) = new_host();
        let c1 = seed(&host, "main", &[("base.txt", "base\n")], "c1");
        set_branch(&host, "feature", c1);
        let feat = seed(&host, "feature", &[("base.txt", "base\n"), ("f.txt", "f\n")], "feat");
        let main = seed(&host, "main", &[("base.txt", "base\n"), ("m.txt", "m\n")], "main2");

        let result = host
            .merge(ORG, REPO, "main", feat, MergeStrategy::Squash, "Ada", "ada@x", "squash")
            .unwrap();
        assert_eq!(result.kind, "squash");
        let squashed = ObjectId::from_hex(&result.merge_commit_sha).unwrap();
        assert_eq!(parents_of(&host, squashed), vec![main]);

        // The squashed tree equals the tree a normal merge would have produced.
        let merge_tree = host.merged_tree(&host.open(ORG, REPO).unwrap(), main, feat).unwrap();
        assert_eq!(tree_of(&host, squashed), merge_tree);
    }

    /// Rebase replays the PR's commits onto base as a linear, single-parent chain.
    #[test]
    fn rebase_replays_commits_linearly() {
        let (_dir, host) = new_host();
        let c1 = seed(&host, "main", &[("base.txt", "base\n")], "c1");
        set_branch(&host, "feature", c1);
        seed(&host, "feature", &[("base.txt", "base\n"), ("f1.txt", "1\n")], "f1");
        let feat = seed(
            &host,
            "feature",
            &[("base.txt", "base\n"), ("f1.txt", "1\n"), ("f2.txt", "2\n")],
            "f2",
        );
        let main = seed(&host, "main", &[("base.txt", "base\n"), ("m.txt", "m\n")], "main2");

        let result = host
            .merge(ORG, REPO, "main", feat, MergeStrategy::Rebase, "Ada", "ada@x", "rebase")
            .unwrap();
        assert_eq!(result.kind, "rebase");
        let tip = ObjectId::from_hex(&result.new_base_sha).unwrap();
        // Linear: tip -> its single parent -> main (two feature commits replayed).
        let p1 = parents_of(&host, tip);
        assert_eq!(p1.len(), 1);
        let p2 = parents_of(&host, p1[0]);
        assert_eq!(p2, vec![main]);
    }

    /// A rebase whose replay conflicts aborts without moving base.
    #[test]
    fn rebase_conflict_aborts() {
        let (_dir, host) = new_host();
        let c1 = seed(&host, "main", &[("base.txt", "base\n")], "c1");
        set_branch(&host, "feature", c1);
        let feat = seed(&host, "feature", &[("base.txt", "feature edit\n")], "feat");
        let main = seed(&host, "main", &[("base.txt", "main edit\n")], "main2");

        let err = host
            .merge(ORG, REPO, "main", feat, MergeStrategy::Rebase, "Ada", "ada@x", "rebase")
            .unwrap_err();
        assert!(matches!(err, crate::GitHostError::Invalid(_)));
        assert_eq!(host.resolve_branch(ORG, REPO, "main").unwrap(), main);
    }

    /// Merging an ancestor is a no-op: reported up-to-date and refused.
    #[test]
    fn up_to_date_when_head_already_in_base() {
        let (_dir, host) = new_host();
        let c1 = seed(&host, "main", &[("base.txt", "base\n")], "c1");
        let main = seed(&host, "main", &[("base.txt", "base\n"), ("m.txt", "m\n")], "c2");

        let analysis = host.mergeability(ORG, REPO, main, c1).unwrap();
        assert_eq!(analysis.status, MergeStatus::UpToDate);

        let err = host
            .merge(ORG, REPO, "main", c1, MergeStrategy::Merge, "Ada", "ada@x", "merge")
            .unwrap_err();
        assert!(matches!(err, crate::GitHostError::Invalid(_)));
    }

    /// `commit_list` returns exactly the commits reachable from head but not base.
    #[test]
    fn commit_list_counts_only_pr_commits() {
        let (_dir, host) = new_host();
        let c1 = seed(&host, "main", &[("base.txt", "base\n")], "c1");
        set_branch(&host, "feature", c1);
        seed(&host, "feature", &[("base.txt", "base\n"), ("a.txt", "a\n")], "add a");
        let feat = seed(
            &host,
            "feature",
            &[("base.txt", "base\n"), ("a.txt", "a\n"), ("b.txt", "b\n")],
            "add b",
        );

        let commits = host.commit_list(ORG, REPO, c1, feat).unwrap();
        assert_eq!(commits.len(), 2);
        let summaries: Vec<&str> = commits.iter().map(|c| c.summary.as_str()).collect();
        assert!(summaries.contains(&"add a") && summaries.contains(&"add b"));
    }

    /// The three-dot PR diff reports a file added on the head side.
    #[test]
    fn pr_diff_reports_added_file() {
        let (_dir, host) = new_host();
        let c1 = seed(&host, "main", &[("base.txt", "base\n")], "c1");
        set_branch(&host, "feature", c1);
        let feat = seed(
            &host,
            "feature",
            &[("base.txt", "base\n"), ("feature.txt", "hello\n")],
            "feat",
        );

        let diff = host.pr_diff(ORG, REPO, c1, feat).unwrap();
        let added: Vec<&str> = diff
            .files
            .iter()
            .filter(|f| f.status == 'A')
            .map(|f| f.path.as_str())
            .collect();
        assert_eq!(added, vec!["feature.txt"]);
        assert!(diff.additions >= 1);
    }
}

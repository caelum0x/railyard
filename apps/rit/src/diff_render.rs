//! Unified-diff command handler: worktree-vs-index, index-vs-HEAD, and
//! tree-vs-tree comparisons rendered as a standard unified diff (`git diff`
//! compatible output).
//!
//! # Entry point
//!
//! [`cmd_diff`] is the single CLI handler. The caller selects the comparison
//! mode through `cached` and `revs`:
//!
//! | `cached` | `revs`            | Comparison                     |
//! |----------|-------------------|--------------------------------|
//! | `false`  | `[]`              | working tree vs index           |
//! | `true`   | `[]`              | index vs HEAD tree              |
//! | `_`      | `["rev1", "rev2"]`| tree at rev1 vs tree at rev2   |
//!
//! # Output format
//!
//! Each changed file produces a block matching `git diff` unified output:
//!
//! ```text
//! diff --git a/<path> b/<path>
//! index <old7>..<new7> <mode>
//! --- a/<path>
//! +++ b/<path>
//! @@ -start,count +start,count @@
//!  context
//! -removed
//! +added
//! ```
//!
//! New-file, deleted-file, and binary blocks follow the same conventions as
//! real Git. Text content is diffed with the `diffy` crate's Myers algorithm.
//!
//! # Bounds
//!
//! Tree traversal is capped at `MAX_DIFF_DEPTH` (256) levels and
//! `MAX_DIFF_ENTRIES` (1,000,000) paths, matching the limits in `diff.rs`.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use crate::diff::{diff_trees, ChangeKind, TreeChange, MAX_DIFF_DEPTH, MAX_DIFF_ENTRIES};
use crate::error::{ForgeError, Result};
use crate::index::Index;
use crate::object::{Object, ObjectType};
use crate::oid::ObjectId;
use crate::repo::Repository;
use crate::worktree::{StatusEntry, WorktreeState};

/// Main entry point for `git diff`.
///
/// - `cached = false, revs = []` — working tree vs index (unstaged changes).
/// - `cached = true,  revs = []` — index vs HEAD (staged changes).
/// - `revs = [rev1, rev2]`       — tree at rev1 vs tree at rev2.
///
/// Output is written to `out` in standard unified diff format.
pub fn cmd_diff(
    repo: &Repository,
    cached: bool,
    revs: &[&str],
    out: &mut impl Write,
) -> Result<()> {
    match (cached, revs) {
        (false, []) => diff_worktree_vs_index(repo, out),
        (true, []) => diff_index_vs_head(repo, out),
        (_, [rev1, rev2]) => diff_two_revs(repo, rev1, rev2, out),
        (_, [rev]) => {
            // Treat a single rev without --cached as a tree-vs-HEAD diff.
            diff_two_revs(repo, rev, "HEAD", out)
        }
        _ => Err(ForgeError::InvalidRef(
            "diff: too many revision arguments (max 2)".into(),
        )),
    }
}

// ── Working tree vs index ─────────────────────────────────────────────────────

/// `git diff` (no flags): compare each tracked working-tree file against its
/// stage-0 index entry. Untracked files are not shown.
fn diff_worktree_vs_index(repo: &Repository, out: &mut impl Write) -> Result<()> {
    let worktree = repo_worktree(repo)?;
    let index = repo.index()?;
    let statuses = repo.tracked_status()?;

    for entry in &statuses {
        if entry.state == WorktreeState::Unmodified {
            continue;
        }
        diff_status_entry(repo, entry, &index, &worktree, out)?;
    }
    Ok(())
}

/// Render the unified diff for one [`StatusEntry`] (worktree vs index).
fn diff_status_entry(
    repo: &Repository,
    entry: &StatusEntry,
    index: &Index,
    worktree: &Path,
    out: &mut impl Write,
) -> Result<()> {
    let old_blob = repo.blob(&entry.index_oid)?;
    let idx_entry = index
        .entries
        .iter()
        .find(|e| e.path == entry.path && e.stage() == 0);
    let mode_str = idx_entry
        .map(|e| format!("{:o}", e.mode))
        .unwrap_or_else(|| "100644".into());

    let (new_data, new_oid): (Option<Vec<u8>>, Option<ObjectId>) = match entry.state {
        WorktreeState::Deleted => (None, None),
        WorktreeState::Unmodified => return Ok(()),
        _ => {
            let rel = path_from_bytes(&entry.path)?;
            let full = worktree.join(&rel);
            let content = std::fs::read(&full)?;
            let oid = crate::object::hash_content(ObjectType::Blob, &content).0;
            (Some(content), Some(oid))
        }
    };

    let (old_mode, new_mode) = match entry.state {
        WorktreeState::Deleted => (Some(mode_str.as_str()), None),
        _ => (Some(mode_str.as_str()), Some(mode_str.as_str())),
    };

    render_file_diff(
        &entry.path,
        Some(entry.index_oid),
        new_oid,
        old_mode,
        new_mode,
        Some(&old_blob.data),
        new_data.as_deref(),
        out,
    )
}

// ── Index vs HEAD ─────────────────────────────────────────────────────────────

/// `git diff --cached`: compare the stage-0 index against the HEAD tree.
/// In an empty repository (HEAD missing) every index entry is treated as an addition.
fn diff_index_vs_head(repo: &Repository, out: &mut impl Write) -> Result<()> {
    let index = repo.index()?;

    // Flatten HEAD tree into path → (mode_string, oid).
    let mut head_map: BTreeMap<Vec<u8>, (String, ObjectId)> = BTreeMap::new();
    match repo.head() {
        Ok(head_oid) => {
            let head_commit = repo.peel_to_commit(head_oid)?;
            flatten_tree(repo, head_commit.tree, Vec::new(), 0, &mut head_map)?;
        }
        Err(ForgeError::InvalidRef(_)) => {} // empty repo — head_map stays empty
        Err(e) => return Err(e),
    }

    // Build index map: path → (mode_string, oid).
    let idx_map: BTreeMap<Vec<u8>, (String, ObjectId)> = index
        .entries
        .iter()
        .filter(|e| e.stage() == 0)
        .map(|e| (e.path.clone(), (format!("{:o}", e.mode), e.oid)))
        .collect();

    // Collect changes in sorted path order.
    let mut all_paths: BTreeMap<&Vec<u8>, ()> = BTreeMap::new();
    for k in head_map.keys() {
        all_paths.insert(k, ());
    }
    for k in idx_map.keys() {
        all_paths.insert(k, ());
    }

    for path in all_paths.keys() {
        let old = head_map.get(*path);
        let new = idx_map.get(*path);
        match (old, new) {
            (None, None) => {}
            (None, Some((new_mode, new_oid))) => {
                // Added to index.
                let new_blob = repo.blob(new_oid)?;
                render_file_diff(
                    path,
                    None,
                    Some(*new_oid),
                    None,
                    Some(new_mode),
                    None,
                    Some(&new_blob.data),
                    out,
                )?;
            }
            (Some((old_mode, old_oid)), None) => {
                // Removed from index (staged deletion).
                let old_blob = repo.blob(old_oid)?;
                render_file_diff(
                    path,
                    Some(*old_oid),
                    None,
                    Some(old_mode),
                    None,
                    Some(&old_blob.data),
                    None,
                    out,
                )?;
            }
            (Some((old_mode, old_oid)), Some((new_mode, new_oid)))
                if old_oid != new_oid || old_mode != new_mode =>
            {
                // Modified or mode-changed.
                let old_blob = repo.blob(old_oid)?;
                let new_blob = repo.blob(new_oid)?;
                render_file_diff(
                    path,
                    Some(*old_oid),
                    Some(*new_oid),
                    Some(old_mode),
                    Some(new_mode),
                    Some(&old_blob.data),
                    Some(&new_blob.data),
                    out,
                )?;
            }
            _ => {} // identical — skip
        }
    }
    Ok(())
}

// ── Tree vs tree ──────────────────────────────────────────────────────────────

/// `git diff <rev1> <rev2>`: compare the trees of two revisions.
fn diff_two_revs(repo: &Repository, rev1: &str, rev2: &str, out: &mut impl Write) -> Result<()> {
    let oid1 = repo.resolve(rev1)?;
    let oid2 = repo.resolve(rev2)?;
    let tree1 = commit_or_tree_oid(repo, oid1)?;
    let tree2 = commit_or_tree_oid(repo, oid2)?;

    let changes = diff_trees(repo.odb(), Some(tree1), Some(tree2))?;
    for change in &changes {
        render_tree_change(repo, change, out)?;
    }
    Ok(())
}

/// Resolve an oid to its tree oid (peel commit or tag; accept raw tree).
fn commit_or_tree_oid(repo: &Repository, oid: ObjectId) -> Result<ObjectId> {
    let mut current = oid;
    for _ in 0..10 {
        match repo.read_object(&current)? {
            Object::Commit(c) => return Ok(c.tree),
            Object::Tree(_) => return Ok(current),
            Object::Tag(t) => current = t.object,
            other => {
                return Err(ForgeError::malformed(
                    "object",
                    format!("{current} is a {}, expected commit or tree", other.object_type()),
                ))
            }
        }
    }
    Err(ForgeError::InvalidRef(format!(
        "{oid} does not peel to a tree within 10 tag layers"
    )))
}

/// Render one [`TreeChange`] entry as a unified diff hunk block.
fn render_tree_change(repo: &Repository, change: &TreeChange, out: &mut impl Write) -> Result<()> {
    let (old_data, new_data): (Option<Vec<u8>>, Option<Vec<u8>>) = match change.kind {
        ChangeKind::Added => {
            let new_blob = repo.blob(change.new_oid.as_ref().expect("Added has new_oid"))?;
            (None, Some(new_blob.data))
        }
        ChangeKind::Deleted => {
            let old_blob = repo.blob(change.old_oid.as_ref().expect("Deleted has old_oid"))?;
            (Some(old_blob.data), None)
        }
        ChangeKind::Modified => {
            let old_blob = repo.blob(change.old_oid.as_ref().expect("Modified has old_oid"))?;
            let new_blob = repo.blob(change.new_oid.as_ref().expect("Modified has new_oid"))?;
            (Some(old_blob.data), Some(new_blob.data))
        }
    };

    render_file_diff(
        &change.path,
        change.old_oid,
        change.new_oid,
        change.old_mode.as_deref(),
        change.new_mode.as_deref(),
        old_data.as_deref(),
        new_data.as_deref(),
        out,
    )
}

// ── Unified diff rendering ────────────────────────────────────────────────────

/// Write the complete unified diff block for one file to `out`.
///
/// Header format matches `git diff`:
/// ```text
/// diff --git a/<path> b/<path>
/// [new file mode <mode>]
/// [deleted file mode <mode>]
/// [index <old7>..<new7> <mode>]
/// --- a/<path>  OR  --- /dev/null
/// +++ b/<path>  OR  +++ /dev/null
/// @@ ... @@
/// ```
///
/// Binary files (content contains a NUL byte) get a `Binary files differ` line
/// instead of hunks. If the file content is identical the block is suppressed.
///
/// `pub(crate)` so that `show.rs` can delegate its per-change rendering here
/// without duplicating the header-formatting logic.
pub(crate) fn render_file_diff(
    path: &[u8],
    old_oid: Option<ObjectId>,
    new_oid: Option<ObjectId>,
    old_mode: Option<&str>,
    new_mode: Option<&str>,
    old_data: Option<&[u8]>,
    new_data: Option<&[u8]>,
    out: &mut impl Write,
) -> Result<()> {
    let path_str = String::from_utf8_lossy(path);

    // ── Binary detection ─────────────────────────────────────────────────────
    let old_binary = old_data.map(is_binary).unwrap_or(false);
    let new_binary = new_data.map(is_binary).unwrap_or(false);

    // ── Build hunk text (diffy) before emitting any headers ──────────────────
    // (so we can skip the block entirely if there are no hunks)
    let hunk_text: Option<String> = if !old_binary && !new_binary {
        let old_str = old_data
            .map(|d| String::from_utf8_lossy(d).into_owned())
            .unwrap_or_default();
        let new_str = new_data
            .map(|d| String::from_utf8_lossy(d).into_owned())
            .unwrap_or_default();
        let patch = diffy::create_patch(&old_str, &new_str);
        let formatted = format!("{patch}");
        // Extract only the @@ hunks — skip diffy's own --- / +++ header lines.
        let body = match formatted.find("@@") {
            Some(pos) => formatted[pos..].to_string(),
            None => String::new(),
        };
        Some(body)
    } else {
        None // binary: don't call diffy
    };

    // If text diff with no hunks, the files are identical — suppress the block.
    let has_hunks = hunk_text.as_deref().map(|s| !s.is_empty()).unwrap_or(true);
    if !has_hunks && old_data.is_some() && new_data.is_some() {
        return Ok(());
    }

    // ── diff --git header ────────────────────────────────────────────────────
    writeln!(out, "diff --git a/{path_str} b/{path_str}")?;

    // ── Mode lines ───────────────────────────────────────────────────────────
    match (old_mode, new_mode) {
        (None, Some(mode)) => writeln!(out, "new file mode {mode}")?,
        (Some(mode), None) => writeln!(out, "deleted file mode {mode}")?,
        (Some(old_m), Some(new_m)) if old_m != new_m => {
            writeln!(out, "old mode {old_m}")?;
            writeln!(out, "new mode {new_m}")?;
        }
        _ => {}
    }

    // ── index line ───────────────────────────────────────────────────────────
    let null_abbrev = "0000000";
    let old_abbrev_str;
    let new_abbrev_str;
    let old_abbrev = match old_oid {
        Some(oid) => {
            old_abbrev_str = oid.to_hex();
            &old_abbrev_str[..7]
        }
        None => null_abbrev,
    };
    let new_abbrev = match new_oid {
        Some(oid) => {
            new_abbrev_str = oid.to_hex();
            &new_abbrev_str[..7]
        }
        None => null_abbrev,
    };
    let mode_for_index = old_mode.or(new_mode).unwrap_or("100644");
    writeln!(out, "index {old_abbrev}..{new_abbrev} {mode_for_index}")?;

    // ── Binary or text body ──────────────────────────────────────────────────
    if old_binary || new_binary {
        writeln!(out, "Binary files a/{path_str} and b/{path_str} differ")?;
        return Ok(());
    }

    // ── --- and +++ lines ───────────────────────────────────────────────────
    match old_data {
        None => writeln!(out, "--- /dev/null")?,
        Some(_) => writeln!(out, "--- a/{path_str}")?,
    }
    match new_data {
        None => writeln!(out, "+++ /dev/null")?,
        Some(_) => writeln!(out, "+++ b/{path_str}")?,
    }

    // ── Hunk body ────────────────────────────────────────────────────────────
    if let Some(body) = hunk_text {
        out.write_all(body.as_bytes())?;
        // Ensure a trailing newline so the next file's header is on its own line.
        if !body.is_empty() && !body.ends_with('\n') {
            writeln!(out)?;
        }
    }

    Ok(())
}

/// Return `true` if `data` contains a NUL byte (heuristic for binary content).
fn is_binary(data: &[u8]) -> bool {
    data.contains(&0u8)
}

// ── Tree flatten (local copy to avoid coupling to diff.rs private internals) ──

/// Recursively flatten a tree into `path → (mode_string, blob_oid)` entries.
///
/// Subtrees are expanded recursively; blobs and symlinks become leaf entries.
/// Depth and entry count are bounded to prevent adversarial graph exhaustion.
fn flatten_tree(
    repo: &Repository,
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
    let tree = match repo.read_object(&tree_oid)? {
        Object::Tree(t) => t,
        other => {
            return Err(ForgeError::malformed(
                "tree",
                format!("expected tree, got {}", other.object_type()),
            ))
        }
    };
    for entry in tree.entries {
        let mut path = prefix.clone();
        if !path.is_empty() {
            path.push(b'/');
        }
        path.extend_from_slice(&entry.name);
        if entry.kind() == "tree" {
            flatten_tree(repo, entry.oid, path, depth + 1, out)?;
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

// ── Utilities ─────────────────────────────────────────────────────────────────

/// Derive the worktree root from the repository's `.git` directory.
///
/// Fails with [`ForgeError::NoWorktree`] for bare repositories.
fn repo_worktree(repo: &Repository) -> Result<PathBuf> {
    let git_dir = repo.git_dir();
    if git_dir
        .file_name()
        .map(|n| n == ".git")
        .unwrap_or(false)
    {
        git_dir
            .parent()
            .map(|p| p.to_path_buf())
            .ok_or_else(|| ForgeError::NoWorktree(git_dir.display().to_string()))
    } else {
        Err(ForgeError::NoWorktree(git_dir.display().to_string()))
    }
}

/// Convert a raw index path (UTF-8, `/`-separated) to a relative [`PathBuf`].
///
/// Rejects paths with non-normal components (no `..`, no absolute, etc.) as a
/// safety guard — index paths are repository-controlled input.
fn path_from_bytes(bytes: &[u8]) -> Result<PathBuf> {
    let s = std::str::from_utf8(bytes)
        .map_err(|_| ForgeError::UnsafePath("non-UTF-8 path in index".into()))?;
    let p = Path::new(s);
    if p.components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return Err(ForgeError::UnsafePath(s.into()));
    }
    Ok(p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo_with_file(
        dir: &Path,
        name: &str,
        content: &[u8],
    ) -> (Repository, ObjectId) {
        let repo = Repository::init(dir, false, Some("main")).unwrap();
        let oid = repo.hash_object(ObjectType::Blob, content, true).unwrap();
        (repo, oid)
    }

    #[test]
    fn diff_identical_trees_produces_no_output() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();
        let blob = repo.hash_object(ObjectType::Blob, b"hello\n", true).unwrap();

        let mut entry = crate::tree::Tree {
            entries: vec![crate::TreeEntry {
                mode: "100644".into(),
                name: b"a.txt".to_vec(),
                oid: blob,
            }],
        };
        entry.canonicalize();
        let tree_oid = repo.hash_object(ObjectType::Tree, &entry.serialize(), true).unwrap();

        let mut out = Vec::new();
        diff_two_revs_with_trees(&repo, tree_oid, tree_oid, &mut out).unwrap();
        assert!(out.is_empty(), "identical trees should produce no diff output");
    }

    fn diff_two_revs_with_trees(
        repo: &Repository,
        t1: ObjectId,
        t2: ObjectId,
        out: &mut Vec<u8>,
    ) -> Result<()> {
        let changes = diff_trees(repo.odb(), Some(t1), Some(t2))?;
        for change in &changes {
            render_tree_change(repo, change, out)?;
        }
        Ok(())
    }

    #[test]
    fn diff_addition_shows_new_file_mode_header() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();
        let blob = repo.hash_object(ObjectType::Blob, b"new content\n", true).unwrap();

        let empty_tree = repo.hash_object(ObjectType::Tree, b"", true).unwrap();
        let mut new_tree_obj = crate::tree::Tree {
            entries: vec![crate::TreeEntry {
                mode: "100644".into(),
                name: b"new.txt".to_vec(),
                oid: blob,
            }],
        };
        new_tree_obj.canonicalize();
        let new_tree = repo.hash_object(ObjectType::Tree, &new_tree_obj.serialize(), true).unwrap();

        let mut out = Vec::new();
        diff_two_revs_with_trees(&repo, empty_tree, new_tree, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("new file mode 100644"), "expected 'new file mode' header: {text}");
        assert!(text.contains("+++ b/new.txt"), "expected +++ header: {text}");
        assert!(text.contains("+new content"), "expected added line: {text}");
    }

    #[test]
    fn diff_deletion_shows_deleted_file_header() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();
        let blob = repo.hash_object(ObjectType::Blob, b"gone\n", true).unwrap();
        let mut old_tree_obj = crate::tree::Tree {
            entries: vec![crate::TreeEntry {
                mode: "100644".into(),
                name: b"gone.txt".to_vec(),
                oid: blob,
            }],
        };
        old_tree_obj.canonicalize();
        let old_tree = repo.hash_object(ObjectType::Tree, &old_tree_obj.serialize(), true).unwrap();
        let empty_tree = repo.hash_object(ObjectType::Tree, b"", true).unwrap();

        let mut out = Vec::new();
        diff_two_revs_with_trees(&repo, old_tree, empty_tree, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("deleted file mode"), "expected 'deleted file mode': {text}");
        assert!(text.contains("-gone"), "expected removed line: {text}");
    }

    #[test]
    fn binary_file_shows_binary_differs() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), true, Some("main")).unwrap();
        let binary = b"\x00\x01\x02\x03binary\x00";
        let old_blob = repo.hash_object(ObjectType::Blob, binary, true).unwrap();
        let new_data = b"\x00changed\x00";
        let new_blob = repo.hash_object(ObjectType::Blob, new_data, true).unwrap();

        let mut out = Vec::new();
        render_file_diff(
            b"bin.dat",
            Some(old_blob),
            Some(new_blob),
            Some("100644"),
            Some("100644"),
            Some(binary),
            Some(new_data),
            &mut out,
        )
        .unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("Binary files"), "expected binary marker: {text}");
    }
}

//! `git switch` / `git checkout <branch-or-commit>` command handlers.
//!
//! Implements the porcelain semantics of `git switch [--create|-c] <target>`
//! and `git checkout <branch-or-oid>`:
//!
//! 1. Resolve `target` to a commit oid, peeling through annotated tags.
//! 2. Determine whether HEAD should be a symbolic ref (branch) or a direct oid
//!    (detached HEAD).
//! 3. Materialize the target commit's tree in the working tree and update the
//!    index via [`Repository::checkout_tree`], which performs the 2-way merge
//!    safety check: any locally modified tracked path that would be altered by
//!    the switch causes an immediate [`ForgeError::WorktreeCollision`] before any
//!    file is touched.
//! 4. Atomically update `.git/HEAD` via the standard Git lock-file protocol.
//!
//! Reflog writes are intentionally out of scope here — the reflog agent owns
//! `.git/logs/`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use crate::error::{ForgeError, Result};
use crate::object::Object;
use crate::oid::ObjectId;
use crate::refs::validate_refname;
use crate::repo::Repository;

/// How HEAD was positioned after a successful [`cmd_switch`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SwitchHead {
    /// HEAD is a symbolic ref pointing at this full ref name (e.g. `refs/heads/main`).
    Symbolic(String),
    /// HEAD is a direct (detached) reference to this commit oid.
    Detached(ObjectId),
}

/// Outcome of a successful [`cmd_switch`] call.
#[derive(Clone, Debug)]
pub struct SwitchResult {
    /// How HEAD was updated.
    pub head: SwitchHead,
    /// Number of files written or removed in the working tree.
    pub files_written: usize,
    /// The commit oid that HEAD now resolves to.
    pub commit_oid: ObjectId,
}

/// Implements `git switch [--create|-c] <target>` and plain `git checkout <branch-or-oid>`.
///
/// | `create` | `target` form                             | Result                                              |
/// |----------|-------------------------------------------|-----------------------------------------------------|
/// | `true`   | short branch name, e.g. `feature`         | creates `refs/heads/feature` at current HEAD, switches |
/// | `false`  | short branch name that exists locally     | symbolic HEAD → `refs/heads/<target>`              |
/// | `false`  | full ref `refs/heads/<name>` that exists  | symbolic HEAD → that ref                           |
/// | `false`  | 40-hex oid, tag, or non-branch ref        | detached HEAD at the peeled commit oid             |
///
/// Returns [`ForgeError::WorktreeCollision`] without touching any file if a
/// locally modified tracked path conflicts with the switch target. Returns
/// [`ForgeError::InvalidRef`] if `create = true` and the branch already exists,
/// or if `target` cannot be resolved.
///
/// **Does not write reflogs** — that is the reflog agent's responsibility.
pub fn cmd_switch(repo: &Repository, target: &str, create: bool) -> Result<SwitchResult> {
    let refs = repo.refs();
    let git_dir = repo.git_dir();

    // ── 1. Determine the target commit oid and the new HEAD mode ─────────────

    let (head, commit_oid) = if create {
        // -c / --create: make a new branch at the current HEAD, then switch.
        let refname = format!("refs/heads/{target}");
        validate_refname(&refname)?;
        if refs.resolve(&refname).is_ok() {
            return Err(ForgeError::InvalidRef(format!(
                "branch '{target}' already exists"
            )));
        }
        let start_oid = repo.head().map_err(|_| {
            ForgeError::InvalidRef("cannot create a branch in an empty repository".into())
        })?;
        refs.compare_and_swap(&refname, None, start_oid)?;
        (SwitchHead::Symbolic(refname), start_oid)
    } else {
        // Resolve target → raw oid (might be a tag, commit, or branch tip).
        let resolved = repo.resolve(target)?;
        // Peel through annotated tags to reach the commit oid.
        let commit_oid = peel_to_commit_oid(repo, resolved)?;

        // Determine whether HEAD should be symbolic or detached.
        // Priority: exact local branch → full refs/heads/... → detached.
        let branch_refname = format!("refs/heads/{target}");
        let head =
            if validate_refname(&branch_refname).is_ok() && refs.resolve(&branch_refname).is_ok() {
                SwitchHead::Symbolic(branch_refname)
            } else if target.starts_with("refs/heads/")
                && validate_refname(target).is_ok()
                && refs.resolve(target).is_ok()
            {
                SwitchHead::Symbolic(target.to_string())
            } else {
                // Raw oid, tag, or non-local-branch ref → detached HEAD.
                SwitchHead::Detached(commit_oid)
            };
        (head, commit_oid)
    };

    // ── 2. Materialize the target tree; check for local-modification conflicts ─

    // checkout_tree does the 2-way merge safety check *before* touching any file.
    let commit = repo.peel_to_commit(commit_oid)?;
    let files_written = repo.checkout_tree(commit.tree)?;

    // ── 3. Atomically update HEAD ────────────────────────────────────────────

    write_head_atomic(git_dir, &head)?;

    Ok(SwitchResult {
        head,
        commit_oid,
        files_written,
    })
}

// ── Private helpers ───────────────────────────────────────────────────────────

/// Peel `oid` through annotated tag objects to the underlying commit oid.
///
/// Returns the **commit oid** (not the Commit struct) so the caller can set
/// HEAD to it directly in detached mode. Tag chains deeper than 10 are
/// rejected as a resource-limit guard against hostile cycles.
fn peel_to_commit_oid(repo: &Repository, oid: ObjectId) -> Result<ObjectId> {
    let mut current = oid;
    for _ in 0..10 {
        match repo.read_object(&current)? {
            Object::Commit(_) => return Ok(current),
            Object::Tag(t) => current = t.object,
            other => {
                return Err(ForgeError::malformed(
                    "object",
                    format!(
                        "{current} is a {}, expected a commit or annotated tag",
                        other.object_type()
                    ),
                ))
            }
        }
    }
    Err(ForgeError::InvalidRef(format!(
        "{oid} does not peel to a commit within 10 tag layers"
    )))
}

/// Write `.git/HEAD` atomically using the standard Git lock-file protocol:
/// write to `HEAD.lock`, fsync, rename over `HEAD`, fsync the directory.
///
/// - Symbolic mode: `ref: <refname>\n`
/// - Detached mode: `<40-hex-oid>\n`
fn write_head_atomic(git_dir: &Path, head: &SwitchHead) -> Result<()> {
    let head_path = git_dir.join("HEAD");
    let lock_path = git_dir.join("HEAD.lock");

    let content = match head {
        SwitchHead::Symbolic(refname) => format!("ref: {refname}\n"),
        SwitchHead::Detached(oid) => format!("{oid}\n"),
    };

    let mut lock = match OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock_path)
    {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            return Err(ForgeError::RefLocked("HEAD".into()))
        }
        Err(error) => return Err(error.into()),
    };

    let result = (|| -> Result<()> {
        lock.write_all(content.as_bytes())?;
        lock.sync_all()?;
        std::fs::rename(&lock_path, &head_path)?;
        if let Some(parent) = head_path.parent() {
            std::fs::File::open(parent)?.sync_all()?;
        }
        Ok(())
    })();

    if result.is_err() {
        let _ = std::fs::remove_file(&lock_path);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::ObjectType;
    use crate::{Signature, WorktreeState};

    fn test_repo() -> (tempfile::TempDir, Repository) {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), false, Some("main")).unwrap();
        (dir, repo)
    }

    fn make_commit(repo: &Repository, message: &str) -> ObjectId {
        let sig_raw = "Test User <test@example.com> 1700000000 +0000";
        let sig = Signature::parse(sig_raw, "test").unwrap();
        // Write an empty blob and stage it so write_tree succeeds.
        let oid = repo
            .hash_object(ObjectType::Blob, b"content", true)
            .unwrap();
        // For a minimal commit, use commit_tree with the empty tree.
        let empty_tree = repo.hash_object(ObjectType::Tree, b"", true).unwrap();
        repo.commit_tree(
            empty_tree,
            vec![],
            sig.clone(),
            sig,
            message.as_bytes(),
            Some("refs/heads/main"),
        )
        .unwrap()
    }

    #[test]
    fn switch_create_makes_branch_and_moves_head() {
        let (_dir, repo) = test_repo();
        // Need at least one commit for HEAD to exist.
        make_commit(&repo, "initial\n");

        let result = cmd_switch(&repo, "feature", true).unwrap();
        assert!(matches!(result.head, SwitchHead::Symbolic(ref n) if n == "refs/heads/feature"));

        // The new branch must exist and resolve to the same commit.
        assert!(repo.refs().resolve("refs/heads/feature").is_ok());
        // HEAD must now be the symbolic ref.
        let head_target = repo.refs().read_head().unwrap();
        assert_eq!(
            head_target,
            crate::RefTarget::Symbolic("refs/heads/feature".into())
        );
    }

    #[test]
    fn switch_create_rejects_existing_branch() {
        let (_dir, repo) = test_repo();
        make_commit(&repo, "initial\n");
        // main already exists.
        let err = cmd_switch(&repo, "main", true).unwrap_err();
        assert!(matches!(err, ForgeError::InvalidRef(_)));
    }

    #[test]
    fn switch_to_existing_branch_is_symbolic() {
        let (_dir, repo) = test_repo();
        make_commit(&repo, "initial\n");
        // Create a second branch.
        let head_oid = repo.head().unwrap();
        repo.refs()
            .compare_and_swap("refs/heads/other", None, head_oid)
            .unwrap();

        let result = cmd_switch(&repo, "other", false).unwrap();
        assert!(matches!(result.head, SwitchHead::Symbolic(ref n) if n == "refs/heads/other"));
    }

    #[test]
    fn switch_to_oid_is_detached() {
        let (_dir, repo) = test_repo();
        make_commit(&repo, "initial\n");
        let oid = repo.head().unwrap();
        let hex = oid.to_hex();

        let result = cmd_switch(&repo, &hex, false).unwrap();
        assert!(matches!(result.head, SwitchHead::Detached(_)));
    }

    #[test]
    fn switch_refuses_locally_modified_path() {
        let dir = tempfile::tempdir().unwrap();
        let repo = Repository::init(dir.path(), false, Some("main")).unwrap();
        let sig = Signature::parse("Test <t@t.com> 1700000001 +0000", "test").unwrap();

        // Write a file and make the first commit on main.
        let data = b"original\n";
        std::fs::write(dir.path().join("file.txt"), data).unwrap();
        // add() expects paths relative to the worktree root.
        repo.add(&[std::path::PathBuf::from("file.txt")]).unwrap();
        repo.commit_index("initial\n", sig.clone(), sig.clone())
            .unwrap();

        // Create "other" branch: same parent commit but tree has different file.txt.
        let new_blob = repo
            .hash_object(ObjectType::Blob, b"different\n", true)
            .unwrap();
        let mut t = crate::Tree {
            entries: vec![crate::TreeEntry {
                mode: "100644".into(),
                name: b"file.txt".to_vec(),
                oid: new_blob,
            }],
        };
        t.canonicalize();
        let new_tree = repo
            .hash_object(ObjectType::Tree, &t.serialize(), true)
            .unwrap();
        let head_oid = repo.head().unwrap();
        repo.commit_tree(
            new_tree,
            vec![head_oid],
            sig.clone(),
            sig,
            b"branch commit\n",
            Some("refs/heads/other"),
        )
        .unwrap();

        // Locally modify file.txt so it conflicts with the switch target.
        std::fs::write(dir.path().join("file.txt"), b"local change\n").unwrap();

        // Switch to "other" should be refused because file.txt is locally modified.
        let err = cmd_switch(&repo, "other", false);
        assert!(err.is_err(), "switch should fail with a local modification");

        // HEAD must still point at main.
        let head_target = repo.refs().read_head().unwrap();
        assert_eq!(
            head_target,
            crate::RefTarget::Symbolic("refs/heads/main".into())
        );
    }
}
